// Demonstrates the difference between regular commit methods and raw commit methods
// Regular commits update references automatically, raw commits just create the object

use anyhow::Context;
use gix::config::tree::{Author, Committer};

fn main() -> anyhow::Result<()> {
    let git_dir = std::env::args_os()
        .nth(1)
        .context("First argument needs to be the directory to initialize the repository in")?;
    let mut repo = gix::init_bare(git_dir)?;

    println!("Repo (bare): {}", repo.git_dir().display());

    // Set up author/committer
    let mut config = repo.config_snapshot_mut();
    config.set_raw_value(&Author::NAME, "Demo User")?;
    config.set_raw_value(&Author::EMAIL, "demo@example.com")?;
    config.set_raw_value(&Committer::NAME, "Demo User")?;
    config.set_raw_value(&Committer::EMAIL, "demo@example.com")?;
    let repo = config.commit_auto_rollback()?;

    let empty_tree_id = repo.write_object(&gix::objs::Tree::empty())?.detach();

    println!("\n=== Demonstrating commit_raw ===");
    
    // Create a raw commit - this doesn't update any references
    let raw_commit = repo.commit_raw("Raw commit message", empty_tree_id, gix::commit::NO_PARENT_IDS)?;
    
    println!("Created raw commit object (not yet written to database):");
    println!("  Message: {}", raw_commit.message);
    println!("  Tree: {}", raw_commit.tree);
    println!("  Author: {:?}", raw_commit.author.name);
    
    // HEAD should still be unborn at this point
    let head_before = match repo.head() {
        Ok(_) => "exists",
        Err(_) => "unborn"
    };
    println!("HEAD status before writing raw commit: {}", head_before);
    
    // Now write the commit object to the database
    let raw_commit_id = repo.write_object(&raw_commit)?;
    println!("Raw commit written to database with ID: {}", raw_commit_id);
    
    // HEAD still shouldn't point to our commit since we didn't update references
    let head_after = match repo.head() {
        Ok(_) => "exists",
        Err(_) => "unborn"
    };
    println!("HEAD status after writing raw commit: {}", head_after);
        
    println!("\n=== Demonstrating commit_as_raw ===");
    
    // Create specific author/committer signatures
    let committer = gix::actor::Signature {
        name: "Committer Name".into(),
        email: "committer@example.com".into(),
        time: gix_date::Time::now_local_or_utc(),
    };
    let author = gix::actor::Signature {
        name: "Author Name".into(),
        email: "author@example.com".into(),
        time: gix_date::Time::now_local_or_utc(),
    };
    
    let raw_commit2 = repo.commit_as_raw(
        committer.to_ref(&mut Default::default()),
        author.to_ref(&mut Default::default()),
        "Second raw commit with custom author/committer",
        empty_tree_id,
        [raw_commit_id.detach()],
    )?;
    
    println!("Created second raw commit with custom author/committer:");
    println!("  Message: {}", raw_commit2.message);
    println!("  Author: {} <{}>", raw_commit2.author.name, raw_commit2.author.email);
    println!("  Committer: {} <{}>", raw_commit2.committer.name, raw_commit2.committer.email);
    
    let raw_commit_id2 = repo.write_object(&raw_commit2)?;
    println!("Second raw commit written with ID: {}", raw_commit_id2);
    
    println!("\n=== Comparing with regular commit ===");
    
    // First, let's update HEAD to point to our second raw commit so we can demonstrate
    // the difference. In practice, you might update references manually.
    println!("To demonstrate regular commit, we first need to set HEAD manually:");
    
    // Use the regular commit method which updates HEAD automatically
    // For the initial commit, we'll use no parents
    let regular_commit_id = repo.commit("HEAD", "Regular commit that updates HEAD", empty_tree_id, gix::commit::NO_PARENT_IDS)?;
    println!("Regular commit created with ID: {}", regular_commit_id);
    
    // Now HEAD should point to our commit
    let head_final = match repo.head() {
        Ok(mut head) => {
            match head.try_peel_to_id_in_place().unwrap_or(None) {
                Some(id) => format!("points to {}", id),
                None => "exists but unborn".to_string(),
            }
        }
        Err(_) => "unborn".to_string()
    };
    println!("HEAD status after regular commit: {}", head_final);
    
    println!("\n=== Summary ===");
    println!("Raw commits allow you to:");
    println!("1. Create commit objects without updating any references");
    println!("2. Write them to the database when you're ready");
    println!("3. Have full control over when and how references are updated");
    println!("4. Batch commit operations for better performance");
    
    Ok(())
}