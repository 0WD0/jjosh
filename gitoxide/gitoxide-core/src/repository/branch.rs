use crate::OutputFormat;

pub enum Kind {
    Local,
    All,
}

impl Kind {
    fn includes_local_branches(&self) -> bool {
        match self {
            Self::Local | Self::All => true,
        }
    }

    fn includes_remote_branches(&self) -> bool {
        match self {
            Self::Local => false,
            Self::All => true,
        }
    }
}

pub struct Options {
    pub kind: Kind,
}

pub fn list(
    repo: gix::Repository,
    out: &mut dyn std::io::Write,
    format: OutputFormat,
    options: Options,
) -> anyhow::Result<()> {
    if format != OutputFormat::Human {
        anyhow::bail!("JSON output isn't supported");
    }

    let platform = repo.references()?;

    if options.kind.includes_local_branches() {
        let mut branch_names: Vec<String> = platform
            .local_branches()?
            .flatten()
            .map(|branch| branch.name().shorten().to_string())
            .collect();

        branch_names.sort();

        for branch_name in branch_names {
            writeln!(out, "{branch_name}")?;
        }
    }

    if options.kind.includes_remote_branches() {
        let mut branch_names: Vec<String> = platform
            .remote_branches()?
            .flatten()
            .map(|branch| branch.name().shorten().to_string())
            .collect();

        branch_names.sort();

        for branch_name in branch_names {
            writeln!(out, "{branch_name}")?;
        }
    }

    Ok(())
}
