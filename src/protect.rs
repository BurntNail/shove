use crate::s3::get_bucket;
use comfy_table::Table;
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, MultiSelect, Password, Select};
use crate::protect::auth_storer::{AuthStorer, Realm};

pub mod auth;
pub mod auth_storer;

pub async fn protect() -> color_eyre::Result<()> {
    let bucket = get_bucket();
    let mut existing_auth = AuthStorer::new(&bucket).await?;


    let theme = ColorfulTheme::default();
    let choice = FuzzySelect::with_theme(&theme)
        .with_prompt("What do you want to do?")
        .items(&[
            "View Current Realms",
            "Remove Existing Realm",
            "View Current Users",
            "Remove Existing User",
            "Add New User",
            "Add New Realm",
            "Set Users with access to Realm",
        ])
        .interact()?;


    match choice {
        0 => {
            let mut table = Table::new();
            table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
            table.set_header(vec!["Pattern", "Usernames"]);

            for (pat, usernames) in existing_auth.get_patterns_and_usernames() {
                table.add_row(vec![format!("{pat:?}"), usernames.join(", ")]);
            }

            println!("{table}");
        }
        1 => {
            let mut patterns_and_usernames = existing_auth.get_patterns_and_usernames();
            if patterns_and_usernames.is_empty() {
                println!("No realms in place.");
                return Ok(());
            }

            let items: Vec<String> = patterns_and_usernames
                .clone()
                .into_iter()
                .map(|(pattern, username)| format!("{pattern:?}: {}", username.join(", ")))
                .collect();
            let choice = FuzzySelect::with_theme(&theme)
                .with_prompt("Which realm to remove?")
                .items(&items)
                .interact()?;

            let pattern_to_remove = patterns_and_usernames.swap_remove(choice).0;

            if Confirm::with_theme(&theme)
                .with_prompt(format!("Confirm removal of {pattern_to_remove:?}"))
                .interact()?
            {
                existing_auth.rm_realm(&pattern_to_remove);
                existing_auth.save(&bucket).await?;
            }
        },
        2 => {
            let mut table = Table::new();
            table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
            table.set_header(vec!["UUID", "Username"]);

            for (uuid, username) in existing_auth.get_users() {
                table.add_row(vec![uuid.to_string(), username]);
            }

            println!("{table}");
        },
        3 => {
            let mut uuids_and_users = existing_auth.get_users();
            if uuids_and_users.is_empty() {
                println!("No users yet.");
                return Ok(());
            }

            let items: Vec<String> = uuids_and_users
                .clone()
                .into_iter()
                .map(|(_, username)| username)
                .collect();
            let choice = FuzzySelect::with_theme(&theme)
                .with_prompt("Which user to remove?")
                .items(&items)
                .interact()?;

            let (uuid, username) = uuids_and_users.swap_remove(choice);

            if Confirm::with_theme(&theme)
                .with_prompt(format!("Confirm removal of {username}"))
                .interact()?
            {
                existing_auth.rm_user(&uuid);
                existing_auth.save(&bucket).await?;
            }
        }
        4 => {
            let username: String = Input::with_theme(&theme)
                .with_prompt("Username?")
                .interact()?;
            let password: String = Password::with_theme(&theme)
                .with_prompt("Password?")
                .with_confirmation("Confirm Password?", "Passwords didn't match.")
                .interact()?;

            let uuid = existing_auth.add_user(username.clone(), password)?;


            let realms = existing_auth.get_all_realms();
            let should_have_access_to = MultiSelect::with_theme(&theme)
                .with_prompt(format!("Which realms should {username:?} have access to?"))
                .items(&realms.iter().map(|x| format!("{x:?}")).collect::<Vec<_>>())
                .interact()?;

            for i in should_have_access_to {
                let pat = realms[i].clone();
                existing_auth.protect_additional(pat, vec![uuid]);
            }

            existing_auth.save(&bucket).await?;
        }
        5 => {
            let pat = Input::with_theme(&theme)
                .with_prompt("Pattern to protect?")
                .interact()?;
            let pat = Realm::StartsWith(pat);

            let uuids = {
                let users = existing_auth.get_users();
                if users.is_empty() {
                    vec![]
                } else {
                    MultiSelect::with_theme(&theme)
                        .with_prompt("Which users should have access to this?")
                        .items(&users.iter().map(|(_, un)| un).collect::<Vec<_>>())
                        .interact()?
                        .into_iter()
                        .flat_map(|x| users.get(x).map(|(uuid, _)| uuid))
                        .copied()
                        .collect()
                }
            };

            existing_auth.protect(pat, uuids);
            existing_auth.save(&bucket).await?;
        }
        6 => {
            let mut patterns: Vec<String> = existing_auth.get_patterns_and_usernames().into_iter().map(|(pat, _)| format!("{pat:?}")).collect();
            if patterns.is_empty() {
                println!("No existing realms.");
                return Ok(());
            }

            let chosen_pat = Select::with_theme(&theme)
                .with_prompt("Which realm?")
                .items(&patterns)
                .interact()?;

            let pat = Realm::StartsWith(patterns.swap_remove(chosen_pat));

            let uuids = {
                let users = existing_auth.get_users();
                if users.is_empty() {
                    vec![]
                } else {
                    let currently_has_access = existing_auth.get_users_with_access_to_realm(&pat);
                    let highlighted: Vec<bool> = users.iter().map(|(uuid, _)| currently_has_access.contains(uuid)).collect();

                    MultiSelect::with_theme(&theme)
                        .with_prompt("Which users should have access to this?")
                        .items(&users.iter().map(|(_, un)| un).collect::<Vec<_>>())
                        .defaults(&highlighted)
                        .interact()?
                        .into_iter()
                        .flat_map(|x| users.get(x).map(|(uuid, _)| uuid))
                        .copied()
                        .collect()
                }
            };

            existing_auth.protect(pat, uuids);
            existing_auth.save(&bucket).await?;
        }
        _ => unreachable!(),
    }

    Ok(())
}
