#![forbid(unsafe_code)]

use gix::Result;

fn main() -> Result<()> {
    gix_tix::command::parse().run()
}
