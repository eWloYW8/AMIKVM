use amikvm_core::media::scsi::Kind;

pub(super) fn image_extensions(kind: Kind) -> Vec<String> {
    let extensions = if kind == Kind::Cdrom {
        ["iso", "nrg"].as_slice()
    } else {
        ["img", "ima", "bin"].as_slice()
    };
    // Native GTK/portal filters use case-sensitive glob patterns. JViewer
    // accepts every ASCII casing, so emit all variants for each extension.
    extensions
        .iter()
        .flat_map(|extension| {
            (0..1 << extension.len()).map(|mask| {
                extension
                    .bytes()
                    .enumerate()
                    .map(|(index, byte)| {
                        if mask & (1 << index) == 0 {
                            char::from(byte)
                        } else {
                            char::from(byte.to_ascii_uppercase())
                        }
                    })
                    .collect()
            })
        })
        .collect()
}
