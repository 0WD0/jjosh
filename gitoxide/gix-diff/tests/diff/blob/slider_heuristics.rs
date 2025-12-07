use gix_diff::blob::{diff_with_slider_heuristics, v0_2};

/// Test basic slider heuristics functionality
#[test]
fn basic_slider_heuristics() -> crate::Result {
    let before = "fn foo() {\n    let x = 1;\n    x\n}\n";
    let after = "fn foo() {\n    let x = 2;\n    x\n}\n";

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    assert_eq!(diff.count_removals(), 1);
    assert_eq!(diff.count_additions(), 1);
    assert!(diff.is_removed(1)); // "    let x = 1;\n" is removed
    assert!(diff.is_added(1)); // "    let x = 2;\n" is added

    Ok(())
}

/// Test that the UnifiedDiffPrinter can be used with the v0.2 API
#[test]
fn unified_diff_printer_usage() -> crate::Result {
    let before = r#"fn foo() {
    let x = 1;
    println!("x = {}", x);
}
"#;

    let after = r#"fn foo() {
    let x = 2;
    println!("x = {}", x);
    println!("done");
}
"#;

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    // Use the UnifiedDiffPrinter to generate a unified diff
    let printer = v0_2::BasicLineDiffPrinter(&input.interner);
    let config = v0_2::UnifiedDiffConfig::default();
    let unified = diff.unified_diff(&printer, config, &input);

    let output = unified.to_string();

    // Verify the output contains expected diff markers
    assert!(output.contains("@@"), "should contain hunk header");
    assert!(output.contains("-    let x = 1;"), "should show removal");
    assert!(output.contains("+    let x = 2;"), "should show addition");
    assert!(output.contains("+    println!(\"done\");"), "should show new line");

    Ok(())
}

/// Test slider heuristics with indentation
#[test]
fn slider_heuristics_with_indentation() -> crate::Result {
    let before = r#"fn main() {
    if true {
        println!("hello");
    }
}
"#;

    let after = r#"fn main() {
    if true {
        println!("hello");
        println!("world");
    }
}
"#;

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    // Verify that only one line was added
    assert_eq!(diff.count_additions(), 1);
    assert_eq!(diff.count_removals(), 0);

    Ok(())
}

/// Test that Myers algorithm also works with slider heuristics
#[test]
fn myers_with_slider_heuristics() -> crate::Result {
    let before = "a\nb\nc\n";
    let after = "a\nx\nc\n";

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Myers, &input);

    assert_eq!(diff.count_removals(), 1);
    assert_eq!(diff.count_additions(), 1);

    Ok(())
}

/// Test empty diff
#[test]
fn empty_diff_with_slider_heuristics() -> crate::Result {
    let before = "unchanged\n";
    let after = "unchanged\n";

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    assert_eq!(diff.count_removals(), 0);
    assert_eq!(diff.count_additions(), 0);

    Ok(())
}

/// Test complex multi-hunk diff with slider heuristics
#[test]
fn multi_hunk_diff_with_slider_heuristics() -> crate::Result {
    let before = r#"struct Foo {
    x: i32,
}

impl Foo {
    fn new() -> Self {
        Foo { x: 0 }
    }
}
"#;

    let after = r#"struct Foo {
    x: i32,
    y: i32,
}

impl Foo {
    fn new() -> Self {
        Foo { x: 0, y: 0 }
    }
}
"#;

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    // Should have additions for the new field and modified constructor
    assert!(diff.count_additions() > 0);
    assert!(diff.count_removals() > 0);

    let printer = v0_2::BasicLineDiffPrinter(&input.interner);
    let config = v0_2::UnifiedDiffConfig::default();
    let unified = diff.unified_diff(&printer, config, &input);

    let output = unified.to_string();

    // Verify the structure
    assert!(output.contains("@@"), "should have hunk headers");
    assert!(output.contains("+    y: i32,"), "should show new field");

    Ok(())
}

/// Test custom context size in UnifiedDiffConfig
#[test]
fn custom_context_size() -> crate::Result {
    let before = "line1\nline2\nline3\nline4\nline5\nline6\nline7\n";
    let after = "line1\nline2\nline3\nMODIFIED\nline5\nline6\nline7\n";

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    let printer = v0_2::BasicLineDiffPrinter(&input.interner);
    
    // Test with context size of 1
    let mut config = v0_2::UnifiedDiffConfig::default();
    config.context_len(1);
    let unified = diff.unified_diff(&printer, config, &input);
    let output_small = unified.to_string();

    // Test with context size of 3 (default)
    let config_default = v0_2::UnifiedDiffConfig::default();
    let unified_default = diff.unified_diff(&printer, config_default, &input);
    let output_default = unified_default.to_string();

    // Smaller context should have fewer lines
    assert!(
        output_small.lines().count() <= output_default.lines().count(),
        "smaller context should have fewer or equal lines"
    );

    Ok(())
}

/// Test that hunks iterator works correctly
#[test]
fn hunks_iterator() -> crate::Result {
    let before = "a\nb\nc\nd\ne\n";
    let after = "a\nX\nc\nY\ne\n";

    let input = v0_2::InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(v0_2::Algorithm::Histogram, &input);

    let hunks: Vec<_> = diff.hunks().collect();
    
    // Should have two separate hunks
    assert_eq!(hunks.len(), 2, "should have two hunks");

    // First hunk: b -> X
    assert_eq!(hunks[0].before.start, 1);
    assert_eq!(hunks[0].before.end, 2);
    assert_eq!(hunks[0].after.start, 1);
    assert_eq!(hunks[0].after.end, 2);

    // Second hunk: d -> Y
    assert_eq!(hunks[1].before.start, 3);
    assert_eq!(hunks[1].before.end, 4);
    assert_eq!(hunks[1].after.start, 3);
    assert_eq!(hunks[1].after.end, 4);

    Ok(())
}

/// Test postprocessing without heuristic
#[test]
fn postprocess_no_heuristic() -> crate::Result {
    let before = "a\nb\nc\n";
    let after = "a\nX\nc\n";

    let input = v0_2::InternedInput::new(before, after);
    
    // Create diff but postprocess without heuristic
    let mut diff = v0_2::Diff::compute(v0_2::Algorithm::Histogram, &input);
    diff.postprocess_no_heuristic(&input);

    assert_eq!(diff.count_removals(), 1);
    assert_eq!(diff.count_additions(), 1);

    Ok(())
}

/// Test that the v0.2 API exposes the IndentHeuristic
#[test]
fn indent_heuristic_available() -> crate::Result {
    let before = "fn foo() {\n    x\n}\n";
    let after = "fn foo() {\n    y\n}\n";

    let input = v0_2::InternedInput::new(before, after);
    
    // Test with custom indent heuristic
    let mut diff = v0_2::Diff::compute(v0_2::Algorithm::Histogram, &input);
    
    // Create custom heuristic
    let heuristic = v0_2::IndentHeuristic::new(|token| {
        let line: &str = &input.interner[token];
        v0_2::IndentLevel::for_ascii_line(line.as_bytes().iter().copied(), 4)
    });
    
    diff.postprocess_with_heuristic(&input, heuristic);
    
    assert_eq!(diff.count_removals(), 1);
    assert_eq!(diff.count_additions(), 1);

    Ok(())
}
