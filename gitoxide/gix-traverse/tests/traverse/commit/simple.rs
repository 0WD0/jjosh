use crate::hex_to_id;
use crate::util::{commit_graph, git_graph, named_fixture, named_fixture_odb, parse_commit_names};
use gix_hash::ObjectId;
use gix_traverse::commit::{simple::Sorting, Parents, Simple};

/// Run a simple traversal and collect the resulting commit IDs.
fn traverse(
    tips: impl IntoIterator<Item = ObjectId>,
    odb: &gix_odb::Handle,
    sorting: Sorting,
    parents: Parents,
    hidden: impl IntoIterator<Item = ObjectId>,
) -> crate::Result<Vec<ObjectId>> {
    let graph = commit_graph(odb.store_ref());
    Simple::new(tips, odb)
        .sorting(sorting)?
        .parents(parents)
        .hide(hidden)?
        .commit_graph(graph)
        .map(|res| res.map(|info| info.id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Run a traversal with both commit-graph enabled and disabled to ensure consistency.
fn traverse_both(
    tips: impl IntoIterator<Item = ObjectId> + Clone,
    odb: &gix_odb::Handle,
    sorting: Sorting,
    parents: Parents,
    hidden: impl IntoIterator<Item = ObjectId> + Clone,
) -> crate::Result<Vec<ObjectId>> {
    // Without commit graph
    let without_graph: Vec<_> = Simple::new(tips.clone(), odb)
        .sorting(sorting)?
        .parents(parents)
        .hide(hidden.clone())?
        .commit_graph(None)
        .map(|res| res.map(|info| info.id))
        .collect::<Result<Vec<_>, _>>()?;

    // With commit graph
    let graph = commit_graph(odb.store_ref());
    let with_graph: Vec<_> = Simple::new(tips, odb)
        .sorting(sorting)?
        .parents(parents)
        .hide(hidden)?
        .commit_graph(graph)
        .map(|res| res.map(|info| info.id))
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(
        without_graph, with_graph,
        "results must be consistent with and without commit-graph"
    );
    Ok(with_graph)
}

fn all_sortings() -> impl Iterator<Item = Sorting> {
    use gix_traverse::commit::simple::CommitTimeOrder;
    [
        Sorting::BreadthFirst,
        Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
        Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
    ]
    .into_iter()
}

mod hide {
    use super::*;

    #[test]
    fn disjoint_hidden_and_interesting() -> crate::Result {
        let (repo_dir, odb) = named_fixture("make_repos.sh", "disjoint_branches")?;

        insta::assert_snapshot!(git_graph(&repo_dir)?, @r"
        * e07cf1277ff7c43090f1acfc85a46039e7de1272  (HEAD -> disjoint) b3
        * 94cf3f3a4c782b672173423e7a4157a02957dd48  b2
        * 34e5ff5ce3d3ba9f0a00d11a7fad72551fff0861  b1
        * b5665181bf4c338ab16b10da0524d81b96aff209  (main) a3
        * f0230ce37b83d8e9f51ea6322ed7e8bd148d8e28  a2
        * 674aca0765b935ac5e7f7e9ab83af7f79272b5b0  a1
        ");

        let tip = hex_to_id("e07cf1277ff7c43090f1acfc85a46039e7de1272"); // b3
        let hidden = [hex_to_id("b5665181bf4c338ab16b10da0524d81b96aff209")]; // a3
        let expected = [
            tip,
            hex_to_id("94cf3f3a4c782b672173423e7a4157a02957dd48"), // b2
            hex_to_id("34e5ff5ce3d3ba9f0a00d11a7fad72551fff0861"), // b1
        ];

        for sorting in all_sortings() {
            let result = traverse_both([tip], &odb, sorting, Parents::All, hidden.clone())?;
            assert_eq!(result, expected, "sorting = {sorting:?}");
        }
        Ok(())
    }

    #[test]
    fn all_hidden() -> crate::Result {
        let (repo_dir, odb) = named_fixture("make_repos.sh", "disjoint_branches")?;

        insta::assert_snapshot!(git_graph(&repo_dir)?, @r"
        * e07cf1277ff7c43090f1acfc85a46039e7de1272  (HEAD -> disjoint) b3
        * 94cf3f3a4c782b672173423e7a4157a02957dd48  b2
        * 34e5ff5ce3d3ba9f0a00d11a7fad72551fff0861  b1
        * b5665181bf4c338ab16b10da0524d81b96aff209  (main) a3
        * f0230ce37b83d8e9f51ea6322ed7e8bd148d8e28  a2
        * 674aca0765b935ac5e7f7e9ab83af7f79272b5b0  a1
        ");

        let tips = [
            hex_to_id("e07cf1277ff7c43090f1acfc85a46039e7de1272"), // b3
            hex_to_id("b5665181bf4c338ab16b10da0524d81b96aff209"), // a3
        ];
        // The start positions are also declared hidden, so nothing should be visible.
        let hidden = tips.clone();

        for sorting in all_sortings() {
            let result = traverse_both(tips.clone(), &odb, sorting, Parents::All, hidden.clone())?;
            assert!(result.is_empty(), "sorting = {sorting:?}");
        }
        Ok(())
    }

    #[test]
    fn some_hidden_and_all_hidden() -> crate::Result {
        let (repo_dir, odb) = named_fixture("make_repos.sh", "simple")?;

        insta::assert_snapshot!(git_graph(&repo_dir)?, @r"
        *-.   f49838d84281c3988eeadd988d97dd358c9f9dc4  (HEAD -> main) merge
        |\ \  
        | | * 48e8dac19508f4238f06c8de2b10301ce64a641c  (branch2) b2c2
        | | * cb6a6befc0a852ac74d74e0354e0f004af29cb79  b2c1
        | * | 66a309480201c4157b0eae86da69f2d606aadbe7  (branch1) b1c2
        | * | 80947acb398362d8236fcb8bf0f8a9dac640583f  b1c1
        | |/  
        * / 0edb95c0c0d9933d88f532ec08fcd405d0eee882  c5
        |/  
        * 8cb5f13b66ce52a49399a2c49f537ee2b812369c  c4
        * 33aa07785dd667c0196064e3be3c51dd9b4744ef  c3
        * ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83  c2
        * 65d6af66f60b8e39fd1ba6a1423178831e764ec5  c1
        ");

        // Test: Hidden has to catch up with non-hidden
        let tip_c2 = hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83");
        let hidden_c5 = hex_to_id("0edb95c0c0d9933d88f532ec08fcd405d0eee882");

        for sorting in all_sortings() {
            let result = traverse_both([tip_c2], &odb, sorting, Parents::All, [hidden_c5])?;
            assert!(
                result.is_empty(),
                "c2 is reachable from hidden c5, sorting = {sorting:?}"
            );
        }

        // Test: merge tip with two branch tips hidden
        let tip_merge = hex_to_id("f49838d84281c3988eeadd988d97dd358c9f9dc4");
        let hidden_branches = [
            hex_to_id("48e8dac19508f4238f06c8de2b10301ce64a641c"), // b2c2
            hex_to_id("66a309480201c4157b0eae86da69f2d606aadbe7"), // b1c2
        ];
        let expected = [
            tip_merge,
            hex_to_id("0edb95c0c0d9933d88f532ec08fcd405d0eee882"), // c5
        ];

        for sorting in all_sortings() {
            let result = traverse_both([tip_merge], &odb, sorting, Parents::All, hidden_branches.clone())?;
            assert_eq!(result, expected, "sorting = {sorting:?}");
        }

        // Test: single-parent mode with hidden catching up
        let tip_b1c1 = hex_to_id("80947acb398362d8236fcb8bf0f8a9dac640583f");
        let hidden_merge = hex_to_id("f49838d84281c3988eeadd988d97dd358c9f9dc4");

        let result = traverse_both([tip_b1c1], &odb, Sorting::BreadthFirst, Parents::First, [hidden_merge])?;
        assert!(result.is_empty(), "b1c1 is reachable from hidden merge");

        Ok(())
    }
}

mod hide_with_graph_painting {
    //! These tests verify th
    //! the relative path lengths between interesting and hidden tips to shared ancestors.
    //!
    //! The implementation must ensure all commits reachable from hidden tips are properly
    //! excluded, regardless of traversal order.

    use super::*;
    use crate::util::fixture;

    #[test]
    fn hidden_tip_with_longer_path_to_shared_ancestor() -> crate::Result {
        // Graph:
        //   A(tip) --> shared
        //            /
        //   H(hidden) --> X --> Y --> shared
        //
        // Expected: only A is returned (shared is reachable from H)
        let dir = fixture("make_repo_for_hidden_bug.sh")?;
        let repo_path = dir.join("long_hidden_path");
        insta::assert_snapshot!(git_graph(&repo_path)?, @"
        * b6cf469d740a02645b7b9f7cdb98977a6cd7e5ab  (HEAD -> main) A
        | * 2955979fbddb1bddb9e1b1ca993789cacf612b18  (hidden_branch) H
        | * ae431c4e51a81a1df4ac22a52c4e247734ee3c9d  X
        | * ab31ef4cacc50169f2b1d753c1e4efd55d570bbc  Y
        |/  
        * f1543941113388f8a194164420fd7da96f73c2ce  shared
        ");
        let odb = gix_odb::at(repo_path.join(".git").join("objects"))?;

        let commits = parse_commit_names(&repo_path)?;
        let tip_a = commits["A"];
        let hidden_h = commits["H"];
        let shared = commits["shared"];

        let expected = vec![tip_a];

        for sorting in all_sortings() {
            let result = traverse([tip_a], &odb, sorting, Parents::All, [hidden_h])?;
            assert_eq!(
                result, expected,
                "sorting = {sorting:?}: 'shared' ({shared}) should NOT be returned because it's \
                 reachable from hidden tip H"
            );
        }

        // Verify against git
        let output = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["rev-list", "main", "--not", "hidden_branch"])
            .output()?;
        let git_output: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| hex_to_id(s.trim()))
            .collect();
        assert_eq!(git_output, expected, "git rev-list should show only A");

        Ok(())
    }

    #[test]
    fn interesting_tip_with_longer_path_to_shared_ancestor() -> crate::Result {
        // Graph:
        //   A(tip) --> B --> C --> D(shared)
        //                        /
        //   H(hidden) --------->+
        //
        // Expected: A, B, C are returned (D is reachable from H)
        let dir = fixture("make_repo_for_hidden_bug.sh")?;
        let repo_path = dir.join("long_interesting_path");
        insta::assert_snapshot!(git_graph(&repo_path)?, @"
        * 8822f888affa916a2c945ef3b17447f29f8aabff  (HEAD -> main) A
        * 90f80e3c031e9149cfa631493663ffe52d645aab  B
        * 2f353d445c4c552eec8e84f0f6f73999d08a8073  C
        | * 7e0cf8f62783a0eb1043fbe56d220308c3e0289e  (hidden_branch) H
        |/  
        * 359b53df58a6e26b95e276a9d1c9e2b33a3b50bf  D
        ");
        let odb = gix_odb::at(repo_path.join(".git").join("objects"))?;

        let commits = parse_commit_names(&repo_path)?;
        let tip_a = commits["A"];
        let hidden_h = commits["H"];
        let d = commits["D"];

        let expected: Vec<_> = ["A", "B", "C"].iter().map(|name| commits[*name]).collect();

        for sorting in all_sortings() {
            let result = traverse([tip_a], &odb, sorting, Parents::All, [hidden_h])?;
            assert_eq!(
                result, expected,
                "sorting = {sorting:?}: 'D' ({d}) should NOT be returned because it's \
                 reachable from hidden tip H"
            );
        }

        // Verify against git
        let output = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["rev-list", "main", "--not", "hidden_branch"])
            .output()?;
        let git_output: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| hex_to_id(s.trim()))
            .collect();
        assert_eq!(git_output, expected, "git rev-list should show A, B, C");

        Ok(())
    }
}

mod different_date_intermixed {
    use super::*;
    use gix_traverse::commit::simple::CommitTimeOrder;

    #[test]
    fn head_breadth_first() -> crate::Result {
        let odb = named_fixture_odb("make_repos.sh", "intermixed")?;
        let tip = hex_to_id("58912d92944087dcb09dca79cdd2a937cc158bed"); // merge

        // This is very different from what git does as it keeps commits together,
        // whereas we spread them out breadth-first.
        let expected = [
            tip,
            hex_to_id("2dce37be587e07caef8c4a5ab60b423b13a8536a"), // c3
            hex_to_id("0f6632a5a7d81417488b86692b729e49c1b73056"), // b1c2
            hex_to_id("a9c28710e058af4e5163699960234adb9fb2abc7"), // b2c2
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("77fd3c6832c0cd542f7a39f3af9250c3268db979"), // b1c1
            hex_to_id("b648f955b930ca95352fae6f22cb593ee0244b27"), // b2c1
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_date_order() -> crate::Result {
        let odb = named_fixture_odb("make_repos.sh", "intermixed")?;
        let tip = hex_to_id("58912d92944087dcb09dca79cdd2a937cc158bed"); // merge

        // NewestFirst - exactly what git shows
        let expected_newest = [
            tip,
            hex_to_id("2dce37be587e07caef8c4a5ab60b423b13a8536a"), // c3
            hex_to_id("0f6632a5a7d81417488b86692b729e49c1b73056"), // b1c2
            hex_to_id("a9c28710e058af4e5163699960234adb9fb2abc7"), // b2c2
            hex_to_id("77fd3c6832c0cd542f7a39f3af9250c3268db979"), // b1c1
            hex_to_id("b648f955b930ca95352fae6f22cb593ee0244b27"), // b2c1
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_newest);

        // OldestFirst
        let expected_oldest = [
            tip,
            hex_to_id("a9c28710e058af4e5163699960234adb9fb2abc7"), // b2c2
            hex_to_id("b648f955b930ca95352fae6f22cb593ee0244b27"), // b2c1
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
            hex_to_id("0f6632a5a7d81417488b86692b729e49c1b73056"), // b1c2
            hex_to_id("77fd3c6832c0cd542f7a39f3af9250c3268db979"), // b1c1
            hex_to_id("2dce37be587e07caef8c4a5ab60b423b13a8536a"), // c3
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_oldest);

        Ok(())
    }
}

mod different_date {
    use super::*;
    use gix_traverse::commit::simple::CommitTimeOrder;

    #[test]
    fn head_breadth_first() -> crate::Result {
        let odb = named_fixture_odb("make_repos.sh", "simple")?;
        let tip = hex_to_id("f49838d84281c3988eeadd988d97dd358c9f9dc4"); // merge

        // This is very different from what git does as it keeps commits together,
        // whereas we spread them out breadth-first.
        let expected = [
            tip,
            hex_to_id("0edb95c0c0d9933d88f532ec08fcd405d0eee882"), // c5
            hex_to_id("66a309480201c4157b0eae86da69f2d606aadbe7"), // b1c2
            hex_to_id("48e8dac19508f4238f06c8de2b10301ce64a641c"), // b2c2
            hex_to_id("8cb5f13b66ce52a49399a2c49f537ee2b812369c"), // c4
            hex_to_id("80947acb398362d8236fcb8bf0f8a9dac640583f"), // b1c1
            hex_to_id("cb6a6befc0a852ac74d74e0354e0f004af29cb79"), // b2c1
            hex_to_id("33aa07785dd667c0196064e3be3c51dd9b4744ef"), // c3
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_date_order() -> crate::Result {
        let odb = named_fixture_odb("make_repos.sh", "simple")?;
        let tip = hex_to_id("f49838d84281c3988eeadd988d97dd358c9f9dc4"); // merge

        // NewestFirst - exactly what git shows
        let expected_newest = [
            tip,
            hex_to_id("0edb95c0c0d9933d88f532ec08fcd405d0eee882"), // c5
            hex_to_id("66a309480201c4157b0eae86da69f2d606aadbe7"), // b1c2
            hex_to_id("80947acb398362d8236fcb8bf0f8a9dac640583f"), // b1c1
            hex_to_id("48e8dac19508f4238f06c8de2b10301ce64a641c"), // b2c2
            hex_to_id("cb6a6befc0a852ac74d74e0354e0f004af29cb79"), // b2c1
            hex_to_id("8cb5f13b66ce52a49399a2c49f537ee2b812369c"), // c4
            hex_to_id("33aa07785dd667c0196064e3be3c51dd9b4744ef"), // c3
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_newest);

        // OldestFirst
        let expected_oldest = [
            tip,
            hex_to_id("48e8dac19508f4238f06c8de2b10301ce64a641c"), // b2c2
            hex_to_id("cb6a6befc0a852ac74d74e0354e0f004af29cb79"), // b2c1
            hex_to_id("8cb5f13b66ce52a49399a2c49f537ee2b812369c"), // c4
            hex_to_id("33aa07785dd667c0196064e3be3c51dd9b4744ef"), // c3
            hex_to_id("ad33ff2d0c4fc77d56b5fbff6f86f332fe792d83"), // c2
            hex_to_id("65d6af66f60b8e39fd1ba6a1423178831e764ec5"), // c1
            hex_to_id("66a309480201c4157b0eae86da69f2d606aadbe7"), // b1c2
            hex_to_id("80947acb398362d8236fcb8bf0f8a9dac640583f"), // b1c1
            hex_to_id("0edb95c0c0d9933d88f532ec08fcd405d0eee882"), // c5
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_oldest);

        Ok(())
    }
}

/// Same dates are somewhat special as they show how sorting-details on priority queues affects ordering
mod same_date {
    use super::*;
    use crate::util::fixture_odb;
    use gix_hash::oid;
    use gix_traverse::commit::simple::CommitTimeOrder;

    fn odb() -> crate::Result<gix_odb::Handle> {
        fixture_odb("make_traversal_repo_for_commits_same_date.sh")
    }

    #[test]
    fn c4_breadth_first() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("9556057aee5abb06912922e9f26c46386a816822"); // c4

        let expected = [
            tip,
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_breadth_first() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"); // m1b1

        // We always take the first parent first, then the second, and so on.
        // Deviation: git for some reason displays b1c2 *before* c5, but I think it's better
        //            to have a strict parent order.
        let expected = [
            tip,
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("ce2e8ffaa9608a26f7b21afc1db89cadb54fd353"), // b1c2
            hex_to_id("9556057aee5abb06912922e9f26c46386a816822"), // c4
            hex_to_id("9152eeee2328073cf23dcf8e90c949170b711659"), // b1c1
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_date_order() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"); // m1b1

        let expected = [
            tip,
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("ce2e8ffaa9608a26f7b21afc1db89cadb54fd353"), // b1c2
            hex_to_id("9556057aee5abb06912922e9f26c46386a816822"), // c4
            hex_to_id("9152eeee2328073cf23dcf8e90c949170b711659"), // b1c1
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected);

        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected);

        Ok(())
    }

    #[test]
    fn head_first_parent_only_breadth_first() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"); // m1b1

        let expected = [
            tip,
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("9556057aee5abb06912922e9f26c46386a816822"), // c4
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::First, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_c4_breadth_first() -> crate::Result {
        let odb = odb()?;
        let tips = [
            hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"), // m1b1
            hex_to_id("9556057aee5abb06912922e9f26c46386a816822"), // c4
        ];

        let expected = [
            tips[0],
            tips[1],
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("ce2e8ffaa9608a26f7b21afc1db89cadb54fd353"), // b1c2
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9152eeee2328073cf23dcf8e90c949170b711659"), // b1c1
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both(tips, &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn filtered_commit_does_not_block_ancestors_reachable_from_another_commit() -> crate::Result {
        // I don't see a use case for the predicate returning false for a commit but return true for
        // at least one of its ancestors, so this test is kind of dubious. But we do want
        // `Ancestors` to not eagerly blacklist all of a commit's ancestors when blacklisting that
        // one commit, and this test happens to check that.
        let odb = odb()?;
        let tip = hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"); // m1b1
        let filter_out = hex_to_id("9152eeee2328073cf23dcf8e90c949170b711659"); // b1c1

        let expected = [
            tip,
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("ce2e8ffaa9608a26f7b21afc1db89cadb54fd353"), // b1c2
            hex_to_id("9556057aee5abb06912922e9f26c46386a816822"), // c4
            hex_to_id("17d78c64cef6c33a10a604573fd2c429e477fd63"), // c3
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let graph = commit_graph(odb.store_ref());
        let result: Vec<_> = Simple::filtered([tip], &odb, move |id: &oid| id != filter_out)
            .sorting(Sorting::BreadthFirst)?
            .parents(Parents::All)
            .hide([])?
            .commit_graph(graph)
            .map(|res| res.map(|info| info.id))
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn predicate_only_called_once_even_if_fork_point() -> crate::Result {
        // The `self.seen` check should come before the `self.predicate` check, as we don't know how
        // expensive calling `self.predicate` may be.
        let odb = odb()?;
        let tip = hex_to_id("01ec18a3ebf2855708ad3c9d244306bc1fae3e9b"); // m1b1
        let filter_out = hex_to_id("9556057aee5abb06912922e9f26c46386a816822"); // c4

        let expected = [
            tip,
            hex_to_id("efd9a841189668f1bab5b8ebade9cd0a1b139a37"), // c5
            hex_to_id("ce2e8ffaa9608a26f7b21afc1db89cadb54fd353"), // b1c2
            hex_to_id("9152eeee2328073cf23dcf8e90c949170b711659"), // b1c1
        ];

        let mut seen = false;
        let graph = commit_graph(odb.store_ref());
        let result: Vec<_> = Simple::filtered([tip], &odb, move |id: &oid| {
            if id == filter_out {
                assert!(!seen, "predicate should only be called once for c4");
                seen = true;
                false
            } else {
                true
            }
        })
        .sorting(Sorting::BreadthFirst)?
        .parents(Parents::All)
        .hide([])?
        .commit_graph(graph)
        .map(|res| res.map(|info| info.id))
        .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(result, expected);
        Ok(())
    }
}

/// Some dates adjusted to be a year apart, but still 'c1' and 'c2' with the same date.
mod adjusted_dates {
    use super::*;
    use crate::util::{fixture, fixture_odb, git_graph};
    use gix_traverse::commit::simple::CommitTimeOrder;

    fn odb() -> crate::Result<gix_odb::Handle> {
        fixture_odb("make_traversal_repo_for_commits_with_dates.sh")
    }

    #[test]
    fn head_breadth_first() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("288e509293165cb5630d08f4185bdf2445bf6170"); // m1b1

        // Here `git` also shows `b1c1` first, making topo-order similar to date order for some reason,
        // even though c2 *is* the first parent.
        let expected = [
            tip,
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        let result = traverse_both([tip], &odb, Sorting::BreadthFirst, Parents::All, [])?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn head_date_order() -> crate::Result {
        let dir = fixture("make_traversal_repo_for_commits_with_dates.sh")?;
        let odb = gix_odb::at(dir.join(".git").join("objects"))?;
        let tip = hex_to_id("288e509293165cb5630d08f4185bdf2445bf6170"); // m1b1

        insta::assert_snapshot!(git_graph(&dir)?, @r"
        *   288e509293165cb5630d08f4185bdf2445bf6170  (HEAD -> main) m1b1
        |\  
        | * bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac  (branch1) b1c1
        * | 9902e3c3e8f0c569b4ab295ddf473e6de763e1e7  c2
        |/  
        * 134385f6d781b7e97062102c6a483440bfda2a03  c1
        ");

        // NewestFirst
        let expected_newest = [
            tip,
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_newest);

        // OldestFirst
        let expected_oldest = [
            tip,
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_oldest);

        Ok(())
    }

    #[test]
    fn head_date_order_with_cutoff() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("288e509293165cb5630d08f4185bdf2445bf6170"); // m1b1

        let expected = [
            tip,
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
        ];

        for order in [CommitTimeOrder::NewestFirst, CommitTimeOrder::OldestFirst] {
            let result = traverse_both(
                [tip],
                &odb,
                Sorting::ByCommitTimeCutoff {
                    order,
                    seconds: 978393600, // =2001-01-02 00:00:00 +0000
                },
                Parents::All,
                [],
            )?;
            assert_eq!(result, expected, "order = {order:?}");
        }
        Ok(())
    }

    #[test]
    fn head_date_order_with_cutoff_disabled() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("288e509293165cb5630d08f4185bdf2445bf6170"); // m1b1
        let very_early = 878393600; // an early date before any commit

        // NewestFirst with early cutoff (effectively disabled)
        let expected_newest = [
            tip,
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTimeCutoff {
                order: CommitTimeOrder::NewestFirst,
                seconds: very_early,
            },
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_newest);

        // OldestFirst with early cutoff
        let expected_oldest = [
            tip,
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
            hex_to_id("bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac"), // b1c1
        ];
        let result = traverse_both(
            [tip],
            &odb,
            Sorting::ByCommitTimeCutoff {
                order: CommitTimeOrder::OldestFirst,
                seconds: very_early,
            },
            Parents::All,
            [],
        )?;
        assert_eq!(result, expected_oldest);

        Ok(())
    }

    #[test]
    fn date_order_with_cutoff_is_applied_to_starting_position() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"); // c2

        for order in [CommitTimeOrder::NewestFirst, CommitTimeOrder::OldestFirst] {
            let graph = commit_graph(odb.store_ref());
            let count = Simple::new([tip], &odb)
                .sorting(Sorting::ByCommitTimeCutoff {
                    order,
                    seconds: 978393600, // =2001-01-02 00:00:00 +0000
                })?
                .commit_graph(graph)
                .count();
            assert_eq!(
                count, 0,
                "initial tips that don't pass cutoff value are not returned either"
            );
        }
        Ok(())
    }

    #[test]
    fn head_date_order_first_parent_only() -> crate::Result {
        let odb = odb()?;
        let tip = hex_to_id("288e509293165cb5630d08f4185bdf2445bf6170"); // m1b1

        let expected = [
            tip,
            hex_to_id("9902e3c3e8f0c569b4ab295ddf473e6de763e1e7"), // c2
            hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"), // c1
        ];

        for order in [CommitTimeOrder::NewestFirst, CommitTimeOrder::OldestFirst] {
            let result = traverse_both([tip], &odb, Sorting::ByCommitTime(order), Parents::First, [])?;
            assert_eq!(result, expected, "order = {order:?}");
        }
        Ok(())
    }
}
