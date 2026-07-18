type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

mod boolean;
mod color;
mod integer;
mod path;
