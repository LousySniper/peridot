//! Battery charge estimation from cell voltage.

/// Generic Li-ion discharge curve used by Logitech mice (mV, percent).
const MOUSE_CURVE: &[(u16, u8)] = &[
    (4186, 100),
    (4067, 90),
    (3989, 80),
    (3922, 70),
    (3859, 60),
    (3811, 50),
    (3778, 40),
    (3751, 30),
    (3717, 20),
    (3671, 10),
    (3646, 5),
    (3579, 2),
    (3500, 0),
];

/// Logitech G headsets sag harder under load; this curve tracks them better.
const HEADSET_CURVE: &[(u16, u8)] = &[
    (4100, 100),
    (3950, 80),
    (3850, 60),
    (3750, 40),
    (3650, 20),
    (3500, 10),
    (3300, 5),
    (3150, 0),
];

fn interpolate(curve: &[(u16, u8)], mv: u16) -> u8 {
    if mv >= curve[0].0 {
        return curve[0].1;
    }
    if mv <= curve[curve.len() - 1].0 {
        return curve[curve.len() - 1].1;
    }
    for w in curve.windows(2) {
        let (vh, ph) = w[0];
        let (vl, pl) = w[1];
        if mv <= vh && mv >= vl {
            let t = (mv - vl) as f64 / (vh - vl) as f64;
            return (pl as f64 + t * (ph as f64 - pl as f64)).round() as u8;
        }
    }
    0
}

pub fn percent_from_voltage(mv: u16, headset: bool) -> u8 {
    interpolate(if headset { HEADSET_CURVE } else { MOUSE_CURVE }, mv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve() {
        assert_eq!(percent_from_voltage(4300, false), 100);
        assert_eq!(percent_from_voltage(3000, false), 0);
        assert_eq!(percent_from_voltage(3811, false), 50);
        let mid = percent_from_voltage(3835, false);
        assert!(mid > 50 && mid < 60, "{}", mid);
        assert_eq!(percent_from_voltage(3850, true), 60);
    }
}
