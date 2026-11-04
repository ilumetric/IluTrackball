//! The Windows pointer settings that shape how motion turns into pixels.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerSettings {
    /// `SPI_GETMOUSESPEED`, 1 to 20.
    pub speed: Option<u32>,
    /// "Enhance pointer precision".
    pub enhanced_precision: Option<bool>,
}

impl PointerSettings {
    /// Pixels per sensor count with acceleration off.
    pub fn multiplier(&self) -> Option<f64> {
        const TABLE: [f64; 20] = [
            0.03125, 0.0625, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0, 1.25, 1.5, 1.75,
            2.0, 2.25, 2.5, 2.75, 3.0, 3.25, 3.5,
        ];
        let speed = self.speed?;
        TABLE.get(speed.checked_sub(1)? as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_follows_the_windows_table() {
        let at = |speed| {
            PointerSettings {
                speed: Some(speed),
                enhanced_precision: None,
            }
            .multiplier()
        };
        assert_eq!(at(1), Some(0.03125));
        assert_eq!(at(10), Some(1.0));
        assert_eq!(at(16), Some(2.5));
        assert_eq!(at(20), Some(3.5));
        assert_eq!(at(0), None);
        assert_eq!(at(21), None);
    }
}
