#![allow(clippy::result_large_err)]
use gix::{Repository, ThreadSafeRepository, open};
pub use gix_testtools::Result;
use gix_testtools::tempfile;
use std::collections::HashMap;

static SHA1_TO_SHA256_HASHES: std::sync::LazyLock<HashMap<&str, &str>> = std::sync::LazyLock::new(|| {
    [
        (
            "288e509293165cb5630d08f4185bdf2445bf6170",
            "10d977a4ec5ad49aff689ea886dd430f5cf2f618811521fa0df2ccb8c69811ae",
        ),
        (
            "9902e3c3e8f0c569b4ab295ddf473e6de763e1e7",
            "bbaf9640a7404a15394dae2606c5090cb44a722be2167d9d78485779aaf4e065",
        ),
        (
            "bcb05040a6925f2ff5e10d3ae1f9264f2e8c43ac",
            "3e87837a4730030fb4a6bf72d121d4e6305df46e7fd93d1f4f3f1b22845112c7",
        ),
        (
            "134385f6d781b7e97062102c6a483440bfda2a03",
            "5c4c31e0551f0d1fb410b7b9366604b050ea3388b96885063f10ba4c3e2dedd0",
        ),
        (
            "3189cd3cb0af8586c39a838aa3e54fd72a872a41",
            "735ec3eb1e74b0815da6d8aeca80ffbffdca25a2b624cc54d5d34caca9bc4dec",
        ),
        (
            "cfa5e6f7872c2f4fed7bd8c3f2732a37536d6912",
            "864c497e9bbd16208a87f7595aff2caa52d96e73436fc008810516ad1d84c29c",
        ),
        (
            "21d3ba9a26b790a4858d67754ae05d04dfce4d0c",
            "95997c02e30a106c5413e7a68e7758c6b3c70e951f7471ee48d75c06edc7d234",
        ),
        (
            "4c3f4cce493d7beb45012e478021b5f65295e5a3",
            "2c309d047b92197ef711ba55ab652c42d36750d6571a3e024a7325e324be3033",
        ),
        (
            "0f35190769db39bc70f60b6fbec9156370ce2f83",
            "d0b9b041a563042e3bae9499e6f0188a69eb4edf454eeb9c839a01ec23d6c4b5",
        ),
        (
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
        ),
        (
            "317e9677c3bcffd006f9fc84bbb0a54ef1676197",
            "1f5807555942aa1bf20804aec2ac2b57ee28543d4c885f6bbc1f574798e6be22",
        ),
        (
            "ce013625030ba8dba906f756967f9e9ca394464a",
            "2cf8d83d9ee29543b34a87727421fdecb7e3f3a183d337639025de576db9ebb4",
        ),
        (
            "fafd9d08a839d99db60b222cd58e2e0bfaf1f7b2",
            "7d8d5a719510afb480e790ecab4c2de8d0aaca041cb2b4b7e7ceb412e77d1cb7",
        ),
        (
            "30887839de28edf7ab66c860e5c58b4d445f6b12",
            "731565bc9052421d78a23136b86c0aa5c0eea176b004dd14374342c99f96c19f",
        ),
        (
            "d8523dfd5a7aa16562fa1c3e1d3b4a4494f97876",
            "07dee378fa15974a711e2353b3a0046b5a0fdeb8ec460ec837e5a868e51baa25",
        ),
        (
            "05dc291f5376cde200316cb0b74b00cfebc79ea4",
            "56ad633ef5fcc8698612ab68dddbba7a03cd2c57e5c12492f6eecc32e899d775",
        ),
        (
            "27e71576a6335294aa6073ab767f8b36bdba81d0",
            "aebc12d1c40fc56c7cd03b6ab527775314fb2ab474a4d63acb40f196b38c0834",
        ),
        (
            "82024b2ef7858273337471cbd1ca1cedbdfd5616",
            "01298ea7e74424e2cd58d829eecda732131e46e14e2b7180f0b11f665d4b7fa2",
        ),
        (
            "b5152869aedeb21e55696bb81de71ea1bb880c85",
            "0ee4bc3d1a4689d554a1c47c2a178a10708782b71482f8d78807680137ad8c5a",
        ),
        (
            "2d9d136fb0765f2e24c44a0f91984318d580d03b",
            "46a5cf85b0fce1f6d1867d557a768d16eac88e3d5461ca167d4e2a87977e6367",
        ),
        (
            "dfd0954dabef3b64f458321ef15571cc1a46d552",
            "e01823bd8c42768f22f0a53c1261a207a101eb967a637f84007fa00d38bc86ca",
        ),
        (
            "f99771fe6a1b535783af3163eba95a927aae21d5",
            "9e987411c18cd9041be08c8021ef7ef96f9e02a73cc42a40b4d04a3b9520a9d7",
        ),
        (
            "e046f3e51d955840619fc7d01fbd9a469663de22",
            "edd23e5e4014be6162b5ae5d10ee2cf709dfbcdb9227f3a3dce6e221927bf2e0",
        ),
        (
            "362cb5539acbd3c8ca355471f97c6a68d3db0da7",
            "5f0ae0c252472dba8c416420b90ce2aead95561489c1f3d46cc1f8c201a8a7e4",
        ),
    ]
    .into()
});

/// Convert a hexadecimal hash into its corresponding `ObjectId` or _panic_.
pub fn hex_to_id(hex: &str) -> gix_hash::ObjectId {
    match gix_testtools::object_hash_from_env().unwrap_or_default() {
        gix_hash::Kind::Sha1 => gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("40 bytes hex"),
        gix_hash::Kind::Sha256 => {
            gix_hash::ObjectId::from_hex(SHA1_TO_SHA256_HASHES.get(hex).copied().unwrap_or(hex).as_bytes())
                .expect("hex object id")
        }
        _ => unimplemented!(),
    }
}

pub fn freeze_time() -> gix_testtools::Env<'static> {
    let frozen_time = "42 +0030";
    gix_testtools::Env::new()
        .unset("GIT_AUTHOR_NAME")
        .unset("GIT_AUTHOR_EMAIL")
        .set("GIT_AUTHOR_DATE", frozen_time)
        .unset("GIT_COMMITTER_NAME")
        .unset("GIT_COMMITTER_EMAIL")
        .set("GIT_COMMITTER_DATE", frozen_time)
}
pub fn repo(name: &str) -> Result<ThreadSafeRepository> {
    let repo_path = gix_testtools::scripted_fixture_read_only(name)?;
    Ok(ThreadSafeRepository::open_opts(repo_path, restricted())?)
}

pub fn repo_opts(name: &str, opts: open::Options) -> std::result::Result<ThreadSafeRepository, open::Error> {
    let repo_path = gix_testtools::scripted_fixture_read_only(name).unwrap();
    ThreadSafeRepository::open_opts(repo_path, opts)
}

pub fn named_repo(name: &str) -> Result<Repository> {
    let repo_path = gix_testtools::scripted_fixture_read_only(name)?;
    Ok(ThreadSafeRepository::open_opts(repo_path, restricted())?.to_thread_local())
}

pub fn named_subrepo_opts(
    fixture: &str,
    name: &str,
    opts: open::Options,
) -> std::result::Result<Repository, gix::open::Error> {
    let repo_path = gix_testtools::scripted_fixture_read_only(fixture)
        .map_err(|err| gix::open::Error::Io(std::io::Error::other(err)))?
        .join(name);
    Ok(ThreadSafeRepository::open_opts(repo_path, opts)?.to_thread_local())
}

pub fn restricted() -> open::Options {
    open::Options::isolated().config_overrides(["user.name=gitoxide", "user.email=gitoxide@localhost"])
}

pub fn restricted_and_git() -> open::Options {
    let mut opts = restricted();
    opts.permissions.env.git_prefix = gix_sec::Permission::Allow;
    opts.permissions.env.identity = gix_sec::Permission::Allow;
    opts
}

pub fn repo_rw(name: &str) -> Result<(Repository, tempfile::TempDir)> {
    repo_rw_opts(name, restricted())
}

pub fn repo_rw_opts(name: &str, opts: gix::open::Options) -> Result<(Repository, tempfile::TempDir)> {
    let repo_path = gix_testtools::scripted_fixture_writable(name)?;
    Ok((
        ThreadSafeRepository::discover_opts(
            repo_path.path(),
            Default::default(),
            gix_sec::trust::Mapping {
                full: opts.clone(),
                reduced: opts,
            },
        )?
        .to_thread_local(),
        repo_path,
    ))
}

pub fn basic_repo() -> Result<Repository> {
    repo("make_basic_repo.sh").map(|r| r.to_thread_local())
}

pub fn basic_rw_repo() -> Result<(Repository, tempfile::TempDir)> {
    repo_rw("make_basic_repo.sh")
}
