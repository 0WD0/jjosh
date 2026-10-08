#[test]
#[cfg(unix)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    ignore = "The test itself uses /proc"
)]
fn umask() -> gix_testtools::TestResult {
    use std::{
        fs::File,
        io::{BufRead, BufReader},
    };

    use bstr::ByteSlice;
    // Check against the umask obtained via a less portable but also completely safe method.
    let less_portable = BufReader::new(File::open("/proc/self/status")?)
        .split(b'\n')
        .find_map(|line| line.map(|l| l.strip_prefix(b"Umask:\t").map(Vec::from)).transpose())
        .expect("has umask line")?
        .to_str()?
        .to_owned();
    let more_portable = format!("{:04o}", gix_testtools::umask());
    assert_eq!(more_portable, less_portable);
    Ok(())
}
