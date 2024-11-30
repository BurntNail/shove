use crate::{protect::auth::AuthChecker, s3::get_bucket};
use comfy_table::Table;
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Password};

pub mod auth;

pub async fn protect() -> color_eyre::Result<()> {
    let theme = ColorfulTheme::default();
    let choice = FuzzySelect::with_theme(&theme)
        .with_prompt("What do you want to do?")
        .items(&[
            "View Current Protections",
            "Remove Existing Protection",
            "Add New Protection",
        ])
        .interact()?;

    let bucket = get_bucket();
    let mut existing_auth = AuthChecker::new(&bucket).await?;

    match choice {
        0 => {
            let mut table = Table::new();
            table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
            table.set_header(vec!["Pattern", "Username"]);

            for (pat, username) in existing_auth.get_patterns_and_usernames().await {
                table.add_row(vec![pat, username]);
            }

            println!("{table}");
        }
        1 => {
            let mut patterns_and_usernames = existing_auth.get_patterns_and_usernames().await;
            if patterns_and_usernames.is_empty() {
                println!("No protections in place.");
            }

            let items: Vec<String> = patterns_and_usernames
                .clone()
                .into_iter()
                .map(|(pattern, username)| format!("{pattern}, {username}"))
                .collect();
            let choice = FuzzySelect::with_theme(&theme)
                .with_prompt("Which protection to remove?")
                .items(&items)
                .interact()?;

            let pattern_to_remove = patterns_and_usernames.swap_remove(choice).0;

            if Confirm::with_theme(&theme)
                .with_prompt(format!("Confirm removal of {pattern_to_remove:?}"))
                .interact()?
            {
                existing_auth.rm_pattern(&pattern_to_remove).await;
                existing_auth.save(&bucket).await?;
            }
        }
        2 => {
            let pattern = Input::with_theme(&theme)
                .with_prompt("Pattern to protect?")
                .interact()?;
            let username = Input::with_theme(&theme)
                .with_prompt("Username?")
                .interact()?;
            let password = Password::new()
                .with_prompt("Password")
                .with_confirmation("Confirm password", "Passwords mismatching")
                .interact()?;

            existing_auth.protect(pattern, username, password).await?;
            existing_auth.save(&bucket).await?;
        }
        _ => unreachable!(),
    }

    Ok(())
}
