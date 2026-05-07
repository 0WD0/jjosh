use serial_test::parallel;

use crate::util::{hex_to_id, named_subrepo_opts};

fn shallow_ids(repo: &gix::Repository) -> crate::Result<Vec<gix::ObjectId>> {
    let commits = repo.shallow_commits()?.expect("present");
    Ok(std::iter::once(commits.head)
        .chain(commits.tail.iter().copied())
        .collect())
}

#[test]
#[parallel]
fn no() -> crate::Result {
    for name in ["base", "empty"] {
        let repo = named_subrepo_opts("make_shallow_repo.sh", name, crate::restricted())?;
        assert!(!repo.is_shallow());
        assert!(repo.shallow_commits()?.is_none());
        let commits: Vec<_> = repo
            .head_id()?
            .ancestors()
            .all()?
            .map(|c| c.map(|c| c.id))
            .collect::<Result<_, _>>()?;
        let expected = if name == "base" {
            vec![
                hex_to_id("30887839de28edf7ab66c860e5c58b4d445f6b12"),
                hex_to_id("d8523dfd5a7aa16562fa1c3e1d3b4a4494f97876"),
                hex_to_id("05dc291f5376cde200316cb0b74b00cfebc79ea4"),
            ]
        } else {
            vec![hex_to_id("05dc291f5376cde200316cb0b74b00cfebc79ea4")]
        };
        assert_eq!(commits, expected);
    }
    Ok(())
}

#[test]
#[parallel]
fn yes() -> crate::Result {
    for name in ["shallow.git", "shallow"] {
        let repo = named_subrepo_opts("make_shallow_repo.sh", name, crate::restricted())?;
        assert!(repo.is_shallow());
        assert_eq!(
            shallow_ids(&repo)?,
            [hex_to_id("30887839de28edf7ab66c860e5c58b4d445f6b12")]
        );
    }
    Ok(())
}

mod traverse {
    use gix_traverse::commit::simple::CommitTimeOrder;
    use serial_test::parallel;

    use crate::util::{hex_to_id, named_subrepo_opts};

    fn sha1_or_sha256_id(sha1: &str, sha256: &str) -> gix::ObjectId {
        let hex = match gix_testtools::object_hash() {
            gix_hash::Kind::Sha1 => sha1,
            gix_hash::Kind::Sha256 => sha256,
            _ => unimplemented!(),
        };
        gix::ObjectId::from_hex(hex.as_bytes()).expect("valid object id")
    }

    #[test]
    #[parallel]
    fn boundary_is_detected_triggering_no_error() -> crate::Result {
        for sorting in [
            gix::revision::walk::Sorting::BreadthFirst,
            gix::revision::walk::Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            gix::revision::walk::Sorting::ByCommitTimeCutoff {
                order: CommitTimeOrder::NewestFirst,
                seconds: 0,
            },
        ] {
            for toggle in [false, true] {
                for name in ["shallow.git", "shallow"] {
                    let repo = named_subrepo_opts("make_shallow_repo.sh", name, crate::restricted())?;
                    let commits: Vec<_> = repo
                        .head_id()?
                        .ancestors()
                        .use_commit_graph(toggle)
                        .sorting(sorting)
                        .all()?
                        .map(|c| c.map(|c| c.id))
                        .collect::<Result<_, _>>()?;
                    assert_eq!(commits, [hex_to_id("30887839de28edf7ab66c860e5c58b4d445f6b12")]);
                }
            }
        }
        Ok(())
    }

    #[test]
    #[parallel]
    fn complex_graphs_can_be_iterated_despite_multiple_shallow_boundaries() -> crate::Result {
        let base = gix_path::realpath(gix_testtools::scripted_fixture_read_only("make_remote_repos.sh")?.join("base"))?;
        let shallow_base = gix_testtools::scripted_fixture_read_only_with_args_single_archive(
            "make_complex_shallow_repo.sh",
            Some(base.to_string_lossy()),
        )?;
        for toggle in [false, true] {
            for name in ["shallow.git", "shallow"] {
                let repo = gix::open_opts(shallow_base.join(name), crate::restricted())?;
                let expected_shallow_ids = if repo.object_hash() == gix_hash::Kind::Sha256 {
                    [
                        sha1_or_sha256_id(
                            "82024b2ef7858273337471cbd1ca1cedbdfd5616",
                            "125ce6c0ed8fe2d20ba96bb2dd9c15a9ef63fcecdee79728f171dc73881aabdd",
                        ),
                        sha1_or_sha256_id(
                            "b5152869aedeb21e55696bb81de71ea1bb880c85",
                            "a5d87b4776ac59907b8a994b23c0ae71cc8bfa3673737e4baf3bb502915300c6",
                        ),
                        sha1_or_sha256_id(
                            "27e71576a6335294aa6073ab767f8b36bdba81d0",
                            "c2eec0d4d46a9d91b6c306fa0a82c993cb244b38fb63696a93f29145ee287684",
                        ),
                    ]
                } else {
                    [
                        sha1_or_sha256_id(
                            "27e71576a6335294aa6073ab767f8b36bdba81d0",
                            "c2eec0d4d46a9d91b6c306fa0a82c993cb244b38fb63696a93f29145ee287684",
                        ),
                        sha1_or_sha256_id(
                            "82024b2ef7858273337471cbd1ca1cedbdfd5616",
                            "125ce6c0ed8fe2d20ba96bb2dd9c15a9ef63fcecdee79728f171dc73881aabdd",
                        ),
                        sha1_or_sha256_id(
                            "b5152869aedeb21e55696bb81de71ea1bb880c85",
                            "a5d87b4776ac59907b8a994b23c0ae71cc8bfa3673737e4baf3bb502915300c6",
                        ),
                    ]
                };
                assert_eq!(super::shallow_ids(&repo)?, expected_shallow_ids);
                let commits: Vec<_> = repo
                    .head_id()?
                    .ancestors()
                    .use_commit_graph(toggle)
                    .sorting(gix::revision::walk::Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
                    .all()?
                    .map(|c| c.map(|c| c.id))
                    .collect::<Result<_, _>>()?;
                assert_eq!(
                    commits,
                    [
                        sha1_or_sha256_id(
                            "f99771fe6a1b535783af3163eba95a927aae21d5",
                            "e36613f63a483f296d306a8c41dc6ad8ecb2f178ab8d0e9c82a130917ae53e65",
                        ),
                        sha1_or_sha256_id(
                            "2d9d136fb0765f2e24c44a0f91984318d580d03b",
                            "1e485b4edcc2040ffdc450396bf67232498ada3abb6c60fc1e35966538b9144f",
                        ),
                        sha1_or_sha256_id(
                            "dfd0954dabef3b64f458321ef15571cc1a46d552",
                            "94c0c58a38f244279fcfc39f909bbb898eb0cecb754965d441fe144059e7207a",
                        ),
                        sha1_or_sha256_id(
                            "b5152869aedeb21e55696bb81de71ea1bb880c85",
                            "a5d87b4776ac59907b8a994b23c0ae71cc8bfa3673737e4baf3bb502915300c6",
                        ),
                        sha1_or_sha256_id(
                            "27e71576a6335294aa6073ab767f8b36bdba81d0",
                            "c2eec0d4d46a9d91b6c306fa0a82c993cb244b38fb63696a93f29145ee287684",
                        ),
                        sha1_or_sha256_id(
                            "82024b2ef7858273337471cbd1ca1cedbdfd5616",
                            "125ce6c0ed8fe2d20ba96bb2dd9c15a9ef63fcecdee79728f171dc73881aabdd",
                        ),
                    ]
                );

                // should be
                // *   f99771f - (HEAD -> main, origin/main, origin/HEAD) A (18 years ago) <A U Thor>
                // | * 2d9d136 - C (18 years ago) <A U Thor>
                // *-. | dfd0954 - (tag: b-tag) B (18 years ago) <A U Thor>
                // | | * 27e7157 - (grafted) F (18 years ago) <A U Thor>
                //     | * b515286 - (grafted) E (18 years ago) <A U Thor>
                //     * 82024b2 - (grafted) D (18 years ago) <A U Thor>
            }
        }
        Ok(())
    }
}
