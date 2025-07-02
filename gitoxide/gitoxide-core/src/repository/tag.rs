pub fn list(repo: gix::Repository, out: &mut dyn std::io::Write) -> anyhow::Result<()> {
    let platform = repo.references()?;

    for mut reference in (platform.tags()?).flatten() {
        let tag = reference.peel_to_tag();
        let tag_ref = tag.as_ref().map(gix::Tag::decode);

        let name = match tag_ref {
            Ok(Ok(tag)) => tag.name.to_string(),
            _ => reference.name().shorten().to_string(),
        };

        writeln!(out, "{name}")?;
    }

    Ok(())
}
