//! The `fennec_cfg` and `fennec_model` flash partitions (see
//! `partitions.csv`).

use std::ffi::CStr;

use anyhow::{Context, bail};
use esp_idf_svc::sys::{
    self, esp, esp_partition_erase_range, esp_partition_find_first, esp_partition_mmap,
    esp_partition_mmap_handle_t, esp_partition_mmap_memory_t_ESP_PARTITION_MMAP_DATA,
    esp_partition_read, esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY, esp_partition_t,
    esp_partition_type_t_ESP_PARTITION_TYPE_DATA, esp_partition_write,
};
use fennec_core::config::Config;
use fennec_core::model::Model;

pub const CONFIG: &CStr = c"fennec_cfg";
pub const MODEL: &CStr = c"fennec_model";

const SECTOR: usize = 4096;

#[derive(Clone, Copy)]
pub struct Partition(&'static esp_partition_t);

// SAFETY: partition descriptors are immutable and live forever.
unsafe impl Send for Partition {}

impl Partition {
    pub fn find(label: &CStr) -> anyhow::Result<Self> {
        let partition = unsafe {
            esp_partition_find_first(
                esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                label.as_ptr(),
            )
        };
        match unsafe { partition.as_ref() } {
            Some(p) => Ok(Self(p)),
            None => bail!(
                "partition {label:?} not found; flash with `fennec flash` so the partition table is written"
            ),
        }
    }

    pub fn size(&self) -> usize {
        self.0.size as usize
    }

    pub fn read(&self, offset: usize, buf: &mut [u8]) -> anyhow::Result<()> {
        esp!(unsafe { esp_partition_read(self.0, offset, buf.as_mut_ptr().cast(), buf.len()) })?;
        Ok(())
    }

    /// Maps the whole partition into the address space. The mapping is never
    /// released.
    pub fn map(&self) -> anyhow::Result<&'static [u8]> {
        let mut ptr = std::ptr::null();
        let mut handle: esp_partition_mmap_handle_t = 0;
        esp!(unsafe {
            esp_partition_mmap(
                self.0,
                0,
                self.size(),
                esp_partition_mmap_memory_t_ESP_PARTITION_MMAP_DATA,
                &mut ptr,
                &mut handle,
            )
        })?;
        Ok(unsafe { std::slice::from_raw_parts(ptr.cast(), self.size()) })
    }

    /// Sequential writer that erases sectors as it goes.
    pub fn writer(self) -> Writer {
        Writer {
            partition: self,
            written: 0,
            erased: 0,
        }
    }
}

pub struct Writer {
    partition: Partition,
    written: usize,
    erased: usize,
}

impl Writer {
    pub fn write(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let end = self.written + data.len();
        if end > self.partition.size() {
            bail!(
                "{} bytes do not fit in a {} byte partition",
                end,
                self.partition.size()
            );
        }
        if end > self.erased {
            let erase_to = end.div_ceil(SECTOR) * SECTOR;
            esp!(unsafe {
                esp_partition_erase_range(self.partition.0, self.erased, erase_to - self.erased)
            })?;
            self.erased = erase_to;
        }
        esp!(unsafe {
            esp_partition_write(
                self.partition.0,
                self.written,
                data.as_ptr().cast(),
                data.len(),
            )
        })?;
        self.written = end;
        Ok(())
    }

    /// Erases the next sector too, so stale data after the end can't be
    /// mistaken for part of the new contents.
    pub fn finish(self) -> anyhow::Result<usize> {
        if self.erased < self.partition.size() {
            esp!(unsafe { esp_partition_erase_range(self.partition.0, self.erased, SECTOR) })?;
        }
        Ok(self.written)
    }
}

/// Reads the device configuration.
pub fn load_config() -> anyhow::Result<Config> {
    let partition = Partition::find(CONFIG)?;
    let mut buf = vec![0u8; SECTOR.min(partition.size())];
    partition.read(0, &mut buf)?;
    Config::decode(&buf).context("cannot load the device configuration")
}

/// Maps the wake word model, if a valid one has been written.
pub fn load_model() -> anyhow::Result<Model<'static>> {
    let bytes = Partition::find(MODEL)?.map()?;
    Ok(Model::from_bytes(bytes).context("no valid wake word model in flash")?)
}

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}
