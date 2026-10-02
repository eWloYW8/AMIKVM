# AMIKVM FAT dependency patch

Source: fatfs 0.3.6, https://crates.io/crates/fatfs/0.3.6 ; upstream https://github.com/rafalh/rust-fatfs . The original MIT license is included.

Changes:

- `src/dir.rs::validate_long_name`: count the 255-unit filename limit in UTF-16, and accept supplementary Unicode scalars encoded as surrogate pairs. The existing VFAT entry writer already writes UTF-16. U+FFFE and U+FFFF remain rejected as reserved UTF-16 values.
- `src/dir.rs::Dir::set_metadata`, `src/dir_entry.rs::DirEntry::set_metadata`, and `DirEntryEditor::set_readonly`: allow callers to set a file or directory's FAT modification timestamp and read-only attribute. AMIKVM applies metadata after closing file/directory handles, with parents last, so exporting contents does not overwrite the source timestamps.

The FAT on-disk format is unchanged. The metadata setters extend the upstream public API; filename validation now accepts valid supplementary Unicode names.

This copy pins the implementation and the patch in the repository. Development examples and external test fixtures from the upstream package are omitted.
