//! One exported function for each kind of value that crosses between Rust and Jade:
//! numbers of every width, `bool`, `char`, text, bytes, arrays, a struct, an
//! optional, and a `Result` whose error the Jade program catches.

/// A struct crosses as a Jade class with public fields, `values.Point`.
#[jade::export]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub name: String,
}

#[jade::export]
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[jade::export]
pub fn is_even(n: i64) -> bool {
    n % 2 == 0
}

#[jade::export]
pub fn next_char(c: char) -> char {
    char::from_u32(c as u32 + 1).unwrap_or(c)
}

/// Every sized number type, added up as a `u64`, which is Jade's `ulong`.
#[jade::export]
pub fn widths(b: u8, s: i16, u: u16, i: i32, w: u32, l: u64) -> u64 {
    b as u64 + s.unsigned_abs() as u64 + u as u64 + i.unsigned_abs() as u64 + w as u64 + l
}

#[jade::export]
pub fn halve(x: f32) -> f32 {
    x / 2.0
}

#[jade::export]
pub fn greet(name: &str) -> String {
    format!("hello, {name}")
}

/// A borrowed result is copied before the argument it borrows from is let go.
#[jade::export]
pub fn first_word(s: &str) -> &str {
    s.split(' ').next().unwrap_or("")
}

#[jade::export]
pub fn reversed(data: &[u8]) -> Vec<u8> {
    data.iter().rev().copied().collect()
}

#[jade::export]
pub fn lengths(words: &[String]) -> Vec<usize> {
    words.iter().map(|w| w.chars().count()).collect()
}

#[jade::export]
pub fn midpoint(a: &Point, b: &Point) -> Point {
    Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0, name: format!("{}-{}", a.name, b.name) }
}

#[jade::export]
pub fn find(words: Vec<String>, prefix: &str) -> Option<String> {
    words.into_iter().find(|w| w.starts_with(prefix))
}

#[jade::export]
pub fn checked_div(a: i64, b: i64) -> Option<i64> {
    a.checked_div(b)
}

/// An `Err` raises its text in the Jade program.
#[jade::export]
pub fn parse_port(s: &str) -> Result<u16, String> {
    s.parse::<u16>().map_err(|e| format!("'{s}' is not a port: {e}"))
}

/// A panic raises too, and never unwinds into the Jade program.
#[jade::export]
pub fn explode(n: i64) -> i64 {
    if n > 0 {
        panic!("{n} is too many");
    }
    n
}
