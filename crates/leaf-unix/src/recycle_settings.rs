//! Linux backend of rf_win::recycle_settings. Status: N/A (the freedesktop Trash has no per-bin size settings; read and write answer an error, not success)

use leaf_common::stub_err;

/// The bin's own settings on the real system: there is no bin (and no such settings) here yet.
// TYPE-COPY(rf-win recycle_settings.rs:78)
pub struct SystemBinSettings;

impl rf_core::recycle_bin::BinSettingsStore for SystemBinSettings {
    fn read(&self) -> anyhow::Result<rf_core::recycle_bin::BinSettings> {
        stub_err!("recycle_settings::SystemBinSettings::read")
    }

    fn write(&self, _changes: &rf_core::recycle_bin::Changes) -> anyhow::Result<()> {
        stub_err!("recycle_settings::SystemBinSettings::write")
    }
}
