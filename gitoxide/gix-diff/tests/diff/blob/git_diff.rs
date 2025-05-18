use gix_diff::blob::intern::InternedInput;
use gix_diff::blob::unified_diff::{ContextSize, NewlineSeparator};
use gix_diff::blob::{
    git_diff::{ChangeGroup, ChangeKind},
    Algorithm, GitDiff, Sink, UnifiedDiff,
};
use std::hash::Hash;

#[test]
fn basic() -> crate::Result {
    let before = r#"struct SomeStruct {
    field1: f64,
    field2: f64,
}

fn main() {
    // Some comment
    let c = SomeStruct { field1: 10.0, field2: 10.0 };

    println!(
        "Print field1 from SomeStruct {}",
        get_field1(&c)
    );
}

fn get_field1(c: &SomeStruct) -> f64 {
    c.field1
}
"#;

    let after = r#"/// This is a struct
struct SomeStruct {
    field1: f64,
    field2: f64,
}

fn main() {
    let c = SomeStruct { field1: 10.0, field2: 10.0 };

    println!(
        "Print field1 and field2 from SomeStruct {} {}",
        get_field1(&c), get_field2(&c)
    );
    println!("Print another line");
}

fn get_field1(c: &SomeStruct) -> f64 {
    c.field1
}

fn get_field2(c: &SomeStruct) -> f64 {
    c.field2
}
"#;
    use crate::blob::git_diff::ChangeKind;

    let expected = &[
        ChangeGroup {
            before: 0..0,
            after: 0..1,
            change_kind: ChangeKind::Added,
        },
        ChangeGroup {
            before: 6..7,
            after: 7..7,
            change_kind: ChangeKind::RemovedBelow,
        },
        ChangeGroup {
            before: 10..12,
            after: 10..12,
            change_kind: ChangeKind::Modified,
        },
        ChangeGroup {
            before: 13..13,
            after: 13..14,
            change_kind: ChangeKind::Added,
        },
        ChangeGroup {
            before: 17..17,
            after: 19..23,
            change_kind: ChangeKind::Added,
        },
    ];

    let input = InternedInput::new(before, after);
    let actual = gix_diff::blob::diff(Algorithm::Histogram, &input, GitDiff::new(&input));
    assert_eq!(actual, expected);
    insta::assert_snapshot!(uni_diff_string(&input, actual), @r#"
    @@ -1,1 +1,2 @@
    +/// This is a struct
     struct SomeStruct {
    @@ -6,3 +7,2 @@
     fn main() {
    -    // Some comment
         let c = SomeStruct { field1: 10.0, field2: 10.0 };
    @@ -10,5 +10,6 @@
         println!(
    -        "Print field1 from SomeStruct {}",
    -        get_field1(&c)
    +        "Print field1 and field2 from SomeStruct {} {}",
    +        get_field1(&c), get_field2(&c)
         );
    +    println!("Print another line");
     }
    @@ -17,2 +19,6 @@
         c.field1
    +
    +fn get_field2(c: &SomeStruct) -> f64 {
    +    c.field2
    +}
     }
    "#);

    let standard_slider = gix_diff::blob::diff(Algorithm::Histogram, &input, uni_diff(&input))?;
    insta::assert_snapshot!(standard_slider, @r#"
    @@ -1,1 +1,2 @@
    +/// This is a struct
     struct SomeStruct {
    @@ -6,3 +7,2 @@
     fn main() {
    -    // Some comment
         let c = SomeStruct { field1: 10.0, field2: 10.0 };
    @@ -10,5 +10,6 @@
         println!(
    -        "Print field1 from SomeStruct {}",
    -        get_field1(&c)
    +        "Print field1 and field2 from SomeStruct {} {}",
    +        get_field1(&c), get_field2(&c)
         );
    +    println!("Print another line");
     }
    @@ -17,2 +18,6 @@
         c.field1
    +}
    +
    +fn get_field2(c: &SomeStruct) -> f64 {
    +    c.field2
     }
    "#);

    let input = InternedInput::new(before.as_bytes(), after.as_bytes());
    let actual = gix_diff::blob::diff(Algorithm::Histogram, &input, GitDiff::new(&input));
    assert_eq!(actual, expected);

    Ok(())
}

fn uni_diff<T: Eq + Hash + AsRef<[u8]>>(input: &InternedInput<T>) -> UnifiedDiff<'_, T, String> {
    UnifiedDiff::new(
        input,
        String::default(),
        NewlineSeparator::AfterHeaderAndLine("\n"),
        ContextSize::symmetrical(1),
    )
}

fn uni_diff_string<T: Eq + Hash + AsRef<[u8]>>(input: &InternedInput<T>, changes: Vec<ChangeGroup>) -> String {
    let mut uni = uni_diff(input);
    for change in changes {
        let (before, after) = change.as_u32_ranges();
        uni.process_change(before, after);
    }
    uni.finish().expect("in-memory is infallible")
}
