#[derive(Debug, Default, PartialEq)]
pub struct Point {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

pub fn shifted(d: i64) -> Point {
    Point { x: d + 1, y: d, ..Default::default() }
}

pub fn corner(d: i64) -> Point {
    Point {
        x: d * 2,
        y: d,
        ..Default::default()
    }
}
