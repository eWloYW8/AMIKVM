//! Transport-local mouse state. Record reports only after a successful write.
#[derive(Default)]
pub struct State {
    last: Option<Vec<u8>>,
}

impl State {
    pub fn written(&mut self, report: &[u8]) {
        debug_assert!(matches!(report.len(), 4 | 6));
        self.last = Some(report.to_vec());
    }

    pub fn release(&self, absolute: bool) -> Option<Vec<u8>> {
        if !absolute {
            return Some(vec![0; 4]);
        }
        // With no absolute position written on this transport, there is no
        // held absolute button to release. Inventing (0, 0) would move the host.
        let mut report = self.last.as_ref().filter(|r| r.len() == 6)?.clone();
        report[0] = 0;
        report[5] = 0;
        Some(report)
    }
}
