use crate::{
    cache_control::manager::{Caching, Directive},
    non_empty_list::NonEmptyList,
    s3::get_bucket,
    Realm,
};
use comfy_table::Table;
use dialoguer::{
    theme::{ColorfulTheme, Theme},
    Confirm, FuzzySelect, Input,
};
use std::num::NonZeroUsize;

pub mod manager;

pub async fn cache() -> color_eyre::Result<()> {
    let bucket = get_bucket();
    let (mut caching, _) = Caching::new(&bucket).await?;

    let theme = ColorfulTheme::default();
    let choice = FuzzySelect::with_theme(&theme)
        .with_prompt("What do you want to do?")
        .items(&["View Caching Rules", "Set Default", "Add New Rule"])
        .interact()?;

    match choice {
        0 => {
            match caching.default.clone() {
                Some(x) => println!("Default Caching: {x:?}"),
                None => println!("Default Caching: Nothing specified"),
            };

            let mut table = Table::new();
            table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
            table.set_header(vec!["Pattern", "Rules"]);

            for (pat, rules) in caching.get_all_caching_rules() {
                if let Some(cc_header) = Directive::directives_to_header(rules.into()) {
                    table.add_row(vec![format!("{pat:?}"), cc_header]);
                }
            }

            println!("{table}");
        }
        1 => {
            caching.default = get_zeroable_directives(&theme)?;
            caching.save(&bucket).await?;
        }
        2 => {
            let pat = Input::with_theme(&theme)
                .with_prompt("What should the path start with?")
                .interact()?;
            let pat = Realm::StartsWith(pat);
            let directives = get_nonempty_directives(&theme)?;

            caching.set_directives(pat, directives);
            caching.save(&bucket).await?;
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn get_zeroable_directives(
    theme: &dyn Theme,
) -> color_eyre::Result<Option<NonEmptyList<Directive>>> {
    Ok(
        if Confirm::with_theme(theme)
            .with_prompt("Would you like any directives?")
            .interact()?
        {
            Some(get_nonempty_directives(theme)?)
        } else {
            None
        },
    )
}

fn get_nonempty_directives(theme: &dyn Theme) -> color_eyre::Result<NonEmptyList<Directive>> {
    let number_of_directives: NonZeroUsize = Input::with_theme(theme)
        .with_prompt("How many directives (must be >0)?")
        .interact()?;
    let number_of_directives: usize = number_of_directives.into();

    let directives = (0..number_of_directives)
        .map(|_| Directive::get_from_stdin(theme))
        .collect::<Result<_, _>>()?;

    Ok(NonEmptyList::new(directives).expect("number of directives should be > 0"))
}
