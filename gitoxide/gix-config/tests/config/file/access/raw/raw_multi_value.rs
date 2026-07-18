use gix_config::{File, lookup};

use crate::file::bstring;

#[test]
fn single_value_is_identical_to_single_value_query() -> crate::Result {
    let config = File::try_from("[core]\na=b\nc=d")?;
    assert_eq!(vec![config.raw_value("core.a")?], config.raw_values("core.a")?);
    Ok(())
}

#[test]
fn multi_value_in_section() -> crate::Result {
    let config = File::try_from("[core]\na=b\na=c")?;
    assert_eq!(config.raw_values("core.a")?, vec![bstring("b"), bstring("c")]);
    Ok(())
}

#[test]
fn multi_value_across_sections() -> crate::Result {
    let config = File::try_from(
        "[core]\n\
         a=b\n\
         a=c\n\
         [core]a=d",
    )?;
    assert_eq!(
        config.raw_values("core.a")?,
        vec![bstring("b"), bstring("c"), bstring("d")]
    );
    Ok(())
}

#[test]
fn values_with_sections_identify_each_values_section_in_file_order() -> crate::Result {
    let config = File::try_from(
        "[core]\n\
         a=b\n\
         a=c\n\
         [core]a=d",
    )?;
    let section_ids: Vec<_> = config.sections().map(|section| section.id()).collect();

    let values = config.raw_values_with_sections("core.a")?;
    let actual: Vec<_> = values
        .into_iter()
        .map(|(value, section)| (value, section.id()))
        .collect();
    assert_eq!(
        actual,
        [
            (bstring("b"), section_ids[0]),
            (bstring("c"), section_ids[0]),
            (bstring("d"), section_ids[1]),
        ]
    );

    let by = config.raw_values_with_sections_by("core", None, "a")?;
    assert_eq!(by.len(), 3, "the explicit-component variant has identical semantics");
    Ok(())
}

#[test]
fn section_not_found() -> crate::Result {
    let config = File::try_from("[core]\na=b\nc=d")?;
    assert!(matches!(
        config.raw_values("foo.a"),
        Err(lookup::existing::Error::SectionMissing)
    ));
    Ok(())
}

#[test]
fn subsection_not_found() -> crate::Result {
    let config = File::try_from("[core]\na=b\nc=d")?;
    assert!(matches!(
        config.raw_values("core.a.a"),
        Err(lookup::existing::Error::SubSectionMissing)
    ));
    Ok(())
}

#[test]
fn key_not_found() -> crate::Result {
    let config = File::try_from("[core]\na=b\nc=d")?;
    assert!(matches!(
        config.raw_values("core.aaaaaa"),
        Err(lookup::existing::Error::KeyMissing)
    ));
    Ok(())
}

#[test]
fn subsection_must_be_respected() -> crate::Result {
    let config = File::try_from("[core]a=b\n[core.a]a=c")?;
    assert_eq!(config.raw_values("core.a")?, vec![bstring("b")]);
    assert_eq!(config.raw_values("core.a.a")?, vec![bstring("c")]);
    Ok(())
}

#[test]
fn non_relevant_subsection_is_ignored() -> crate::Result {
    let config = File::try_from("[core]\na=b\na=c\n[core]a=d\n[core]g=g")?;
    assert_eq!(
        config.raw_values("core.a")?,
        vec![bstring("b"), bstring("c"), bstring("d")]
    );
    Ok(())
}
