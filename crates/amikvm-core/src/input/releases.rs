//! Pair native hardware releases with the press actually delivered by a toolkit.
//! Keep physical cycles separate even when their millisecond timestamps coincide.
pub struct Releases<T> {
    sequence: u64,
    frames: Vec<Frame<T>>,
}
struct Frame<T> {
    hardware: u16,
    time: u32,
    order: Option<u64>,
    released: Option<(u32, u64)>,
    snapshot: Option<T>,
}
impl<T> Default for Releases<T> {
    fn default() -> Self {
        Self {
            sequence: 0,
            frames: vec![],
        }
    }
}
impl<T> Releases<T> {
    fn next(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
    pub fn pressed(&mut self, hardware: u16, time: u32) {
        let order = self.next();
        if let Some(frame) = self.frames.iter_mut().find(|f| {
            f.hardware == hardware && f.time == time && f.order.is_none() && f.released.is_none()
        }) {
            frame.order = Some(order);
        } else {
            self.frames.push(Frame {
                hardware,
                time,
                order: Some(order),
                released: None,
                snapshot: None,
            });
        }
    }
    pub fn released(&mut self, hardware: u16, time: u32) {
        let order = self.next();
        if let Some(frame) = self
            .frames
            .iter_mut()
            .find(|f| f.hardware == hardware && f.released.is_none())
        {
            frame.released = Some((time, order));
        }
    }
    /// Return earlier missing releases before this new toolkit press is delivered.
    pub fn capture(&mut self, hardware: u16, time: u32, snapshot: T) -> Vec<(T, u32)> {
        let order = self
            .frames
            .iter()
            .find(|f| f.hardware == hardware && f.time == time && f.snapshot.is_none())
            .and_then(|f| f.order);
        let previous = self.take(|released, released_order| {
            order.map_or_else(
                || time != released && time.wrapping_sub(released) < (1 << 31),
                |order| order.wrapping_sub(released_order) < (1 << 63),
            )
        });
        if let Some(frame) = self
            .frames
            .iter_mut()
            .find(|f| f.hardware == hardware && f.time == time && f.snapshot.is_none())
        {
            frame.snapshot = Some(snapshot);
        } else if !self.frames.iter().any(|f| f.hardware == hardware) {
            // A keyboard can become available while the toolkit is delivering
            // its first press; attach its later raw release to this snapshot.
            self.frames.push(Frame {
                hardware,
                time,
                order: None,
                released: None,
                snapshot: Some(snapshot),
            });
        }
        previous
    }
    /// A normal toolkit release already delivers this cycle; never duplicate it.
    pub fn delivered(&mut self, hardware: u16, time: u32) {
        let exact = self
            .frames
            .iter()
            .position(|f| f.hardware == hardware && f.released.is_some_and(|(t, _)| t == time));
        let index = exact.or_else(|| {
            self.frames.iter().position(|f| {
                f.hardware == hardware
                    && f.released.is_none()
                    && time.wrapping_sub(f.time) < (1 << 31)
            })
        });
        if let Some(index) = index {
            self.frames.remove(index);
        }
    }
    /// Drain after the toolkit's pending events have been dispatched.
    pub fn drain(&mut self) -> Vec<(T, u32)> {
        self.take(|_, _| true)
    }
    /// Finish a lost device's delivered presses without inventing key-downs.
    /// Preserve actual release order, then unwind held chords in reverse order.
    pub fn cancel(&mut self, time: u32) -> Vec<(T, u32)> {
        let mut releases = self.drain();
        releases.extend(
            self.frames
                .drain(..)
                .rev()
                .filter_map(|frame| frame.snapshot.map(|snapshot| (snapshot, time))),
        );
        releases
    }
    fn take(&mut self, before: impl Fn(u32, u64) -> bool) -> Vec<(T, u32)> {
        let mut releases = vec![];
        let mut keep = Vec::with_capacity(self.frames.len());
        for frame in self.frames.drain(..) {
            if let Some((time, order)) = frame.released.filter(|(t, o)| before(*t, *o)) {
                if let Some(snapshot) = frame.snapshot {
                    releases.push((snapshot, time, order));
                }
            } else {
                keep.push(frame);
            }
        }
        self.frames = keep;
        releases.sort_by_key(|(_, _, order)| std::cmp::Reverse(self.sequence.wrapping_sub(*order)));
        releases
            .into_iter()
            .map(|(snapshot, time, _)| (snapshot, time))
            .collect()
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}
