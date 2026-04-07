use std::mem::swap;

use expect_test::expect;

use crate::intern::InternedInput;
use crate::{diff, Algorithm, UnifiedDiffBuilder};

#[test]
fn replace() {
    let before = r#"fn foo() -> Bar{
    let mut foo = 2.0;
    foo *= 100 / 2;
    println!("hello world")        
}"#;

    let after = r#"const TEST: i32 = 0;
fn foo() -> Bar{
    let mut foo = 2.0;
    foo *= 100 / 2;
    println!("hello world");        
    println!("hello foo {TEST}");        
}
    
"#;
    let input = InternedInput::new(before, after);
    for algorithm in Algorithm::ALL {
        println!("{algorithm:?}");
        let diff = diff(algorithm, &input, UnifiedDiffBuilder::new(&input));
        expect![[r#"
            @@ -1,5 +1,8 @@
            +const TEST: i32 = 0;
             fn foo() -> Bar{
                 let mut foo = 2.0;
                 foo *= 100 / 2;
            -    println!("hello world")        
            +    println!("hello world");        
            +    println!("hello foo {TEST}");        
             }
            +    
        "#]]
        .assert_eq(&diff);
    }
}

#[test]
fn identical_files() {
    let file = r#"fn foo() -> Bar{
    let mut foo = 2.0;
    foo *= 100 / 2;
}"#;

    for algorithm in Algorithm::ALL {
        println!("{algorithm:?}");
        let input = InternedInput::new(file, file);
        let diff = diff(algorithm, &input, UnifiedDiffBuilder::new(&input));
        assert_eq!(diff, "");
    }
}

#[test]
fn simple_insert() {
    let before = r#"fn foo() -> Bar{
    let mut foo = 2.0;
    foo *= 100 / 2;
}"#;

    let after = r#"fn foo() -> Bar{
    let mut foo = 2.0;
    foo *= 100 / 2;
    println("hello world")
}"#;

    let mut input = InternedInput::new(before, after);
    for algorithm in Algorithm::ALL {
        println!("{algorithm:?}");
        let res = diff(algorithm, &input, UnifiedDiffBuilder::new(&input));
        expect![[r#"
          @@ -1,4 +1,5 @@
           fn foo() -> Bar{
               let mut foo = 2.0;
               foo *= 100 / 2;
          +    println("hello world")
           }
          "#]]
        .assert_eq(&res);

        swap(&mut input.before, &mut input.after);

        let res = diff(algorithm, &input, UnifiedDiffBuilder::new(&input));
        expect![[r#"
            @@ -1,5 +1,4 @@
             fn foo() -> Bar{
                 let mut foo = 2.0;
                 foo *= 100 / 2;
            -    println("hello world")
             }
            "#]]
        .assert_eq(&res);

        swap(&mut input.before, &mut input.after);
    }
}

#[test]
#[cfg(not(miri))]
fn hand_checked_udiffs() {
    let before = r#"use crate::{
    alpha::Alpha,
    beta::Beta,
    gamma::Gamma,
};

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub struct Engine {
    cache: HashMap<String, usize>,
    steps: Vec<&'static str>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            steps: vec!["parse", "render"],
        }
    }

    pub fn update(&mut self, path: &Path) {
        let _ = path;
        self.steps.push("scan");
    }
}

fn unchanged_one() {
    println!("one");
}

fn unchanged_two() {
    println!("two");
}

pub enum Error {
    InvalidPath,
    Unknown,
}

pub struct Layer {
    pub depth: usize,
}

impl Layer {
    pub fn parse(&self) -> Result<(), Error> {
        Ok(())
    }
}
"#;
    let after = r#"use crate::{
    alpha::Alpha,
    beta::Beta,
    gamma::Gamma,
};

use std::{
    collections::HashMap,
    mem::replace,
    path::Path,
};

pub struct Engine {
    cache: HashMap<String, usize>,
    steps: Vec<&'static str>,
    dirty: bool,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            steps: vec!["parse", "render"],
            dirty: false,
        }
    }

    pub fn update(&mut self, path: &Path) {
        let _previous = replace(&mut self.dirty, true);
        let _ = path;
        self.steps.push("scan");
    }
}

fn unchanged_one() {
    println!("one");
}

fn unchanged_two() {
    println!("two");
}

pub enum Error {
    InvalidPath,
    InvalidState,
    Unknown,
}

pub struct Layer {
    pub depth: u32,
}

impl Layer {
    pub fn parse(&self) -> Result<(), Error> {
        Ok(())
    }
}
"#;

    for algorithm in Algorithm::ALL {
        println!("{algorithm:?}");
        let input = InternedInput::new(before, after);
        let diff = diff(algorithm, &input, UnifiedDiffBuilder::new(&input));
        expect![[r#"
@@ -5,13 +5,15 @@
 };
 
 use std::{
-    collections::{HashMap, HashSet},
+    collections::HashMap,
+    mem::replace,
     path::Path,
 };
 
 pub struct Engine {
     cache: HashMap<String, usize>,
     steps: Vec<&'static str>,
+    dirty: bool,
 }
 
 impl Engine {
@@ -19,10 +21,12 @@
         Self {
             cache: HashMap::new(),
             steps: vec!["parse", "render"],
+            dirty: false,
         }
     }
 
     pub fn update(&mut self, path: &Path) {
+        let _previous = replace(&mut self.dirty, true);
         let _ = path;
         self.steps.push("scan");
     }
@@ -38,11 +42,12 @@
 
 pub enum Error {
     InvalidPath,
+    InvalidState,
     Unknown,
 }
 
 pub struct Layer {
-    pub depth: usize,
+    pub depth: u32,
 }
 
 impl Layer {
"#]]
        .assert_eq(&diff);
    }
}
