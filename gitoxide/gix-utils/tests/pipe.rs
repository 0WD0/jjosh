mod io {
    use std::io::{BufRead, ErrorKind, Read, Write};

    use gix_utils::io;

    #[test]
    fn threaded_read_to_end() -> gix_testtools::TestResult {
        let (mut writer, mut reader) = gix_utils::io::pipe::unidirectional(0);

        let message = "Hello, world!";
        std::thread::spawn(move || {
            writer
                .write_all(message.as_bytes())
                .expect("writes to work if reader is present");
        });

        let mut received = String::new();
        reader.read_to_string(&mut received)?;

        assert_eq!(&received, message);
        Ok(())
    }

    #[test]
    fn lack_of_reader_fails_with_broken_pipe() {
        let (mut writer, _) = io::pipe::unidirectional(0);
        let err = writer.write_all(b"must fail").expect_err("the operation must fail");
        insta::assert_debug_snapshot!(gix_testtools::redact_debug_snapshot(&err, &[]), "lack of reader fails with broken pipe", @"
        Custom {
            kind: BrokenPipe,
            error: SendError { .. },
        }
        ");
        assert_eq!(err.kind(), ErrorKind::BrokenPipe);
    }
    #[test]
    fn line_reading_one_by_one() -> gix_testtools::TestResult {
        let (mut writer, mut reader) = io::pipe::unidirectional(2);
        writer.write_all(b"a\n")?;
        writer.write_all(b"b\nc")?;
        drop(writer);
        let mut buf = String::new();
        for expected in &["a\n", "b\n", "c"] {
            buf.clear();
            assert_eq!(reader.read_line(&mut buf)?, expected.len());
            assert_eq!(buf, *expected);
        }
        Ok(())
    }

    #[test]
    fn line_reading() -> gix_testtools::TestResult {
        let (mut writer, reader) = io::pipe::unidirectional(2);
        writer.write_all(b"a\n")?;
        writer.write_all(b"b\nc\n")?;
        drop(writer);
        assert_eq!(reader.lines().collect::<Result<Vec<_>, _>>()?, vec!["a", "b", "c"]);
        Ok(())
    }

    #[test]
    fn writer_can_inject_errors() -> gix_testtools::TestResult {
        let (writer, mut reader) = io::pipe::unidirectional(1);
        writer.channel.send(Err(std::io::Error::other("the error")))?;
        let mut buf = [0];
        insta::assert_debug_snapshot!(reader.read(&mut buf).expect_err("using Read trait, errors are propagated"), "using Read trait, errors are propagated", @r#"
        Custom {
            kind: Other,
            error: "the error",
        }
        "#);

        writer.channel.send(Err(std::io::Error::other("the error")))?;
        insta::assert_debug_snapshot!(reader.fill_buf().expect_err("using BufRead trait, errors are propagated"), "using BufRead trait, errors are propagated", @r#"
        Custom {
            kind: Other,
            error: "the error",
        }
        "#);
        Ok(())
    }

    #[test]
    fn continue_on_empty_writes() -> gix_testtools::TestResult {
        let (mut writer, mut reader) = io::pipe::unidirectional(2);
        writer.write_all(&[])?;
        let input = b"hello";
        writer.write_all(input)?;
        let mut buf = vec![0u8; input.len()];
        assert_eq!(reader.read(&mut buf)?, input.len());
        assert_eq!(buf, &input[..]);
        Ok(())
    }

    #[test]
    fn small_reads() {
        const BLOCK_SIZE: usize = 20;
        let block_count = 20;
        let (mut writer, mut reader) = io::pipe::unidirectional(4);
        std::thread::spawn(move || {
            for _ in 0..block_count {
                let data = &[0; BLOCK_SIZE];
                writer.write_all(data).expect("reader remains connected");
            }
        });

        let mut small_read_buf = [0; BLOCK_SIZE / 2];
        let mut bytes_read = 0;
        while let Ok(size) = reader.read(&mut small_read_buf) {
            if size == 0 {
                break;
            }
            bytes_read += size;
        }
        assert_eq!(block_count * BLOCK_SIZE, bytes_read);
    }
}
