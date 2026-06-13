use crate::{Error, Result};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

/// Fixed-size page storage used by the ORAM bucket layer.
pub trait PageStore {
    /// Size in bytes of one page.
    fn page_size(&self) -> usize;

    /// Number of pages.
    fn page_count(&self) -> usize;

    /// Read a page into `out`.
    fn read_page(&mut self, page_idx: usize, out: &mut [u8]) -> Result<()>;

    /// Write a page from `input`.
    fn write_page(&mut self, page_idx: usize, input: &[u8]) -> Result<()>;

    /// Flush durable storage. In-memory implementations may no-op.
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<T: PageStore + ?Sized> PageStore for Box<T> {
    fn page_size(&self) -> usize {
        (**self).page_size()
    }

    fn page_count(&self) -> usize {
        (**self).page_count()
    }

    fn read_page(&mut self, page_idx: usize, out: &mut [u8]) -> Result<()> {
        (**self).read_page(page_idx, out)
    }

    fn write_page(&mut self, page_idx: usize, input: &[u8]) -> Result<()> {
        (**self).write_page(page_idx, input)
    }

    fn flush(&mut self) -> Result<()> {
        (**self).flush()
    }
}

/// In-memory page store for tests and microbenchmarks.
#[derive(Debug)]
pub struct MemPageStore {
    page_size: usize,
    pages: Vec<u8>,
}

impl MemPageStore {
    /// Create a zero-filled in-memory store.
    pub fn new(page_count: usize, page_size: usize) -> Result<Self> {
        if page_count == 0 || page_size == 0 {
            return Err(Error::InvalidInput(
                "page_count and page_size must be > 0".into(),
            ));
        }
        Ok(Self {
            page_size,
            pages: vec![0; page_count * page_size],
        })
    }
}

impl PageStore for MemPageStore {
    fn page_size(&self) -> usize {
        self.page_size
    }

    fn page_count(&self) -> usize {
        self.pages.len() / self.page_size
    }

    fn read_page(&mut self, page_idx: usize, out: &mut [u8]) -> Result<()> {
        check_page(self, page_idx, out.len())?;
        let start = page_idx * self.page_size;
        out.copy_from_slice(&self.pages[start..start + self.page_size]);
        Ok(())
    }

    fn write_page(&mut self, page_idx: usize, input: &[u8]) -> Result<()> {
        check_page(self, page_idx, input.len())?;
        let start = page_idx * self.page_size;
        self.pages[start..start + self.page_size].copy_from_slice(input);
        Ok(())
    }
}

/// File-backed fixed-size page store.
#[derive(Debug)]
pub struct FilePageStore {
    page_size: usize,
    page_count: usize,
    file: File,
}

impl FilePageStore {
    /// Open or create a fixed-size page file.
    ///
    /// Existing contents are preserved if the file already has the requested
    /// length. Otherwise the file is resized and newly allocated bytes are zero.
    pub fn open(path: impl AsRef<Path>, page_count: usize, page_size: usize) -> Result<Self> {
        if page_count == 0 || page_size == 0 {
            return Err(Error::InvalidInput(
                "page_count and page_size must be > 0".into(),
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.set_len((page_count * page_size) as u64)?;
        Ok(Self {
            page_size,
            page_count,
            file,
        })
    }

    fn offset(&self, page_idx: usize) -> Result<u64> {
        if page_idx >= self.page_count {
            return Err(Error::InvalidInput(format!(
                "page_idx {} out of range {}",
                page_idx, self.page_count
            )));
        }
        Ok((page_idx * self.page_size) as u64)
    }
}

impl PageStore for FilePageStore {
    fn page_size(&self) -> usize {
        self.page_size
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    fn read_page(&mut self, page_idx: usize, out: &mut [u8]) -> Result<()> {
        check_page(self, page_idx, out.len())?;
        self.file.seek(SeekFrom::Start(self.offset(page_idx)?))?;
        self.file.read_exact(out)?;
        Ok(())
    }

    fn write_page(&mut self, page_idx: usize, input: &[u8]) -> Result<()> {
        check_page(self, page_idx, input.len())?;
        self.file.seek(SeekFrom::Start(self.offset(page_idx)?))?;
        self.file.write_all(input)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
}

/// A wrapper that records page access traces for tests.
#[derive(Debug)]
pub struct TracingStore<S> {
    inner: S,
    trace: RefCell<Vec<TraceEvent>>,
}

/// One page access observed by the storage layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceEvent {
    /// Page read.
    Read(usize),
    /// Page write.
    Write(usize),
}

impl<S> TracingStore<S> {
    /// Wrap a page store.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            trace: RefCell::new(Vec::new()),
        }
    }

    /// Return and clear the trace.
    pub fn take_trace(&self) -> Vec<TraceEvent> {
        std::mem::take(&mut *self.trace.borrow_mut())
    }

    /// Consume the wrapper.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: PageStore> PageStore for TracingStore<S> {
    fn page_size(&self) -> usize {
        self.inner.page_size()
    }

    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    fn read_page(&mut self, page_idx: usize, out: &mut [u8]) -> Result<()> {
        self.trace.borrow_mut().push(TraceEvent::Read(page_idx));
        self.inner.read_page(page_idx, out)
    }

    fn write_page(&mut self, page_idx: usize, input: &[u8]) -> Result<()> {
        self.trace.borrow_mut().push(TraceEvent::Write(page_idx));
        self.inner.write_page(page_idx, input)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

fn check_page(store: &impl PageStore, page_idx: usize, len: usize) -> Result<()> {
    if page_idx >= store.page_count() {
        return Err(Error::InvalidInput(format!(
            "page_idx {} out of range {}",
            page_idx,
            store.page_count()
        )));
    }
    if len != store.page_size() {
        return Err(Error::InvalidInput(format!(
            "page buffer len {} != page_size {}",
            len,
            store.page_size()
        )));
    }
    Ok(())
}
