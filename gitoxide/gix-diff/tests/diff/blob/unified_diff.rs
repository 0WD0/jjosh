use gix_diff::blob::unified_diff::{ConsumeHunk, ContextSize, NewlineSeparator};
use gix_diff::blob::{Algorithm, UnifiedDiff};

#[test]
fn removed_modified_added() -> crate::Result {
    let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10";
    let b = "2\n3\n4\n5\nsix\n7\n8\n9\n10\neleven\ntwelve";

    let interner = gix_diff::blob::intern::InternedInput::new(a, b);
    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndLine("\n"),
            ContextSize::symmetrical(3),
        ),
    )?;

    // merged by context.
    insta::assert_snapshot!(actual, @r"
    @@ -1,10 +1,11 @@
    -1
     2
     3
     4
     5
    -6
    +six
     7
     8
     9
     10
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndLine("\n"),
            ContextSize::symmetrical(1),
        ),
    )?;
    // Small context lines keeps hunks separate.
    insta::assert_snapshot!(actual, @r"
    @@ -1,2 +1,1 @@
    -1
     2
    @@ -5,3 +4,3 @@
     5
    -6
    +six
     7
    @@ -10,1 +9,3 @@
     10
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndLine("\n"),
            ContextSize::symmetrical(0),
        ),
    )?;
    // No context is also fine
    insta::assert_snapshot!(actual, @r"
    @@ -1,1 +1,0 @@
    -1
    @@ -6,1 +5,1 @@
    -6
    +six
    @@ -11,0 +10,2 @@
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            Recorder::default(),
            NewlineSeparator::AfterHeaderAndLine("\n"),
            ContextSize::symmetrical(1),
        ),
    )?;
    assert_eq!(
        actual,
        &[
            ((1, 2), (1, 1), "@@ -1,2 +1,1 @@\n".to_string()),
            ((5, 3), (4, 3), "@@ -5,3 +4,3 @@\n".into()),
            ((10, 1), (9, 3), "@@ -10,1 +9,3 @@\n".into())
        ]
    );

    Ok(())
}

#[test]
fn removed_modified_added_with_newlines_in_tokens() -> crate::Result {
    let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10";
    let b = "2\n3\n4\n5\nsix\n7\n8\n9\n10\neleven\ntwelve";

    let a = gix_diff::blob::sources::lines_with_terminator(a);
    let b = gix_diff::blob::sources::lines_with_terminator(b);
    let interner = gix_diff::blob::intern::InternedInput::new(a, b);
    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndWhenNeeded("\n"),
            ContextSize::symmetrical(3),
        ),
    )?;

    // merged by context.
    // newline diffs differently.
    insta::assert_snapshot!(actual, @r"
    @@ -1,10 +1,11 @@
    -1
     2
     3
     4
     5
    -6
    +six
     7
     8
     9
    -10
    +10
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndWhenNeeded("\n"),
            ContextSize::symmetrical(1),
        ),
    )?;
    // Small context lines keeps hunks separate.
    insta::assert_snapshot!(actual, @r"
    @@ -1,2 +1,1 @@
    -1
     2
    @@ -5,3 +4,3 @@
     5
    -6
    +six
     7
    @@ -9,2 +8,4 @@
     9
    -10
    +10
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndWhenNeeded("\n"),
            ContextSize::symmetrical(0),
        ),
    )?;
    // No context is also fine
    insta::assert_snapshot!(actual, @r"
    @@ -1,1 +1,0 @@
    -1
    @@ -6,1 +5,1 @@
    -6
    +six
    @@ -10,1 +9,3 @@
    -10
    +10
    +eleven
    +twelve
    ");

    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            Recorder::default(),
            NewlineSeparator::AfterHeaderAndWhenNeeded("\r\n"),
            ContextSize::symmetrical(1),
        ),
    )?;
    assert_eq!(
        actual,
        &[
            ((1, 2), (1, 1), "@@ -1,2 +1,1 @@\r\n".to_string()),
            ((5, 3), (4, 3), "@@ -5,3 +4,3 @@\r\n".into()),
            ((9, 2), (8, 4), "@@ -9,2 +8,4 @@\r\n".into())
        ]
    );

    Ok(())
}

#[test]
fn all_added_or_removed() -> crate::Result {
    let content = "1\n2\n3\n4\n5";

    let samples = [0, 1, 3, 100];
    for context_lines in samples {
        let interner = gix_diff::blob::intern::InternedInput::new("", content);
        let actual = gix_diff::blob::diff(
            Algorithm::Myers,
            &interner,
            UnifiedDiff::new(
                &interner,
                String::new(),
                NewlineSeparator::AfterHeaderAndLine("\n"),
                ContextSize::symmetrical(context_lines),
            ),
        )?;
        assert_eq!(
            actual,
            r#"@@ -1,0 +1,5 @@
+1
+2
+3
+4
+5
"#,
            "context lines don't matter here"
        );
    }

    for context_lines in samples {
        let interner = gix_diff::blob::intern::InternedInput::new(content, "");
        let actual = gix_diff::blob::diff(
            Algorithm::Myers,
            &interner,
            UnifiedDiff::new(
                &interner,
                String::new(),
                NewlineSeparator::AfterHeaderAndLine("\n"),
                ContextSize::symmetrical(context_lines),
            ),
        )?;
        assert_eq!(
            actual,
            r"@@ -1,5 +1,0 @@
-1
-2
-3
-4
-5
",
            "context lines don't matter here"
        );
    }
    Ok(())
}

#[test]
fn empty() -> crate::Result {
    let interner = gix_diff::blob::intern::InternedInput::new(&b""[..], &b""[..]);
    let actual = gix_diff::blob::diff(
        Algorithm::Myers,
        &interner,
        UnifiedDiff::new(
            &interner,
            String::new(),
            NewlineSeparator::AfterHeaderAndLine("\n"),
            ContextSize::symmetrical(3),
        ),
    )?;

    insta::assert_snapshot!(actual, @r"");
    Ok(())
}

#[derive(Default)]
struct Recorder {
    #[allow(clippy::type_complexity)]
    hunks: Vec<((u32, u32), (u32, u32), String)>,
}

impl ConsumeHunk for Recorder {
    type Out = Vec<((u32, u32), (u32, u32), String)>;

    fn consume_hunk(
        &mut self,
        before_hunk_start: u32,
        before_hunk_len: u32,
        after_hunk_start: u32,
        after_hunk_len: u32,
        header: &str,
        _hunk: &[u8],
    ) -> std::io::Result<()> {
        self.hunks.push((
            (before_hunk_start, before_hunk_len),
            (after_hunk_start, after_hunk_len),
            header.to_string(),
        ));
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.hunks
    }
}
