#![feature(iter_intersperse)]
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::Index;
use std::str::FromStr;

use clap::StructOpt;
use color_eyre::Result;
use color_eyre::{
    eyre::{eyre, Context},
    Help,
};
use date_time_parser::DateParser;
use notion::ids::{AsIdentifier, DatabaseId, PropertyId};
use notion::models::properties::{
    Color as NotionColor, DateOrDateTime, DateValue, PropertyConfiguration, PropertyValue,
};
use notion::models::text::{RichText, RichTextCommon};
use notion::models::{PageCreate, Parent};
use notion::{models::search::NotionSearch, NotionApi};
use owo_colors::colors::css;
use owo_colors::{OwoColorize, Style};
use serde::{Deserialize, Serialize};
use tokio::fs::read_to_string;
use tokio::io;
use tracing::field;

async fn get_config_item(name: &str) -> Result<String> {
    let mut path = dirs::config_dir().ok_or(eyre!("couldn't get config dir"))?;
    path.push("notion-entry");
    path.push(name);
    read_to_string(&path)
        .await
        .context("reading config")
        .with_suggestion(|| format!("consider creating {:?}", &path))
        .map(|tok| tok.trim().to_string())
}

type Props = HashMap<String, PropertyConfiguration>;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct PropertyReadConfig {
    order: Vec<PropertyId>,
    default_values: HashMap<PropertyId, PropertyValue>,
}

struct PropEntryCommandDef {
    name: &'static str,
    usage: &'static str,
    exec: Box<dyn Fn(&str, &mut PropertyReadConfig) -> Result<()>>,
}

enum PropEntryCommand {
    /// Reorder it in the next config
    Order(usize),
    /// Set default to whatever is parsed from this
    Default(String),
}

fn colour_to_owo(colour: NotionColor) -> Style {
    let sty = Style::default();
    match colour {
        NotionColor::Default => sty,
        NotionColor::Gray => sty.fg::<css::Gray>(),
        NotionColor::Brown => sty.fg::<css::Brown>(),
        NotionColor::Orange => sty.fg::<css::Orange>(),
        NotionColor::Yellow => sty.fg::<css::Yellow>(),
        NotionColor::Green => sty.fg::<css::Green>(),
        NotionColor::Blue => sty.fg::<css::LightBlue>(),
        NotionColor::Purple => sty.fg::<css::Purple>(),
        NotionColor::Pink => sty.fg::<css::Pink>(),
        NotionColor::Red => sty.fg::<css::Red>(),
    }
}

type ParseContinuation = Box<dyn FnOnce(&str) -> Option<PropertyValue>>;

fn prompt_for(name: &str, cfg: PropertyConfiguration) -> Option<(String, ParseContinuation)> {
    Some(match cfg {
        PropertyConfiguration::Text { id } => (
            format!("{name}:"),
            Box::new(move |input| {
                Some(PropertyValue::Text {
                    id,
                    rich_text: vec![RichText::from_plain_text(input)],
                })
            }),
        ),
        PropertyConfiguration::Title { id } => (
            format!("{name}:"),
            Box::new(move |input| {
                Some(PropertyValue::Title {
                    id,
                    title: vec![RichText::from_plain_text(input)],
                })
            }),
        ),
        PropertyConfiguration::Select { id, select } => (
            select
                .options
                .iter()
                .enumerate()
                .map(|(n, opt)| format!("{n}: {}", opt.name.style(colour_to_owo(opt.color))))
                .intersperse(", ".to_string())
                .collect(),
            Box::new(move |input| {
                let opt = input
                    .trim()
                    .parse()
                    .ok()
                    .and_then(|n: usize| select.options.get(n).cloned());
                Some(PropertyValue::Select { id, select: opt })
            }),
        ),
        PropertyConfiguration::Date { id } => (
            name.to_string(),
            Box::new(move |input| {
                if input.trim() != "" {
                    let parsed = DateParser::parse(input)?;
                    Some(PropertyValue::Date {
                        id,
                        date: Some(DateValue {
                            start: DateOrDateTime::Date(parsed),
                            end: None,
                        }),
                    })
                } else {
                    Some(PropertyValue::Date { id, date: None })
                }
            }),
        ),
        PropertyConfiguration::Url { id } => (
            name.to_string(),
            Box::new(move |input| {
                let input = input.trim();
                if input != "" {
                    Some(PropertyValue::Url {
                        id,
                        url: Some(input.to_string()),
                    })
                } else {
                    Some(PropertyValue::Url { id, url: None })
                }
            }),
        ),
        _ => return None,
    })
}

fn get_props_from_user(
    props: Props,
    cfg: PropertyReadConfig,
) -> (Vec<PropertyValue>, PropertyReadConfig) {
    let mut cfg = cfg.clone();

    // order now has all of the props
    let have_props = cfg.order.iter().cloned().collect::<HashSet<_>>();
    for prop in props.values() {
        if !have_props.contains(prop.as_id()) {
            cfg.order.push(prop.as_id().clone());
        }
    }

    let mut next_cfg = cfg.clone();

    let cmds = [PropEntryCommandDef {
        name: "order",
        usage: ":order N\nwhere N is the 0-indexed position it should be in",
        exec: Box::new(|args, cf| Ok(())),
    }];

    let mut outs = Vec::new();

    'props: for prop in cfg.order.iter() {
        'oneprop: loop {
            let prop_def = props.iter().find(|(_name, v)| v.as_id() == prop);
            let (name, prop_def) = match prop_def {
                Some(v) => v,
                None => {
                    tracing::warn!(?prop, "prop does not exist?!");
                    continue 'props;
                }
            };

            let prompt = prompt_for(name.as_str(), prop_def.clone());
            let (prompt, continue_parse) = match prompt {
                Some(v) => v,
                None => {
                    tracing::debug!(?prop_def, "prop type unsupported");
                    continue 'props;
                }
            };
            println!("{}", prompt);
            print!("> ");
            std::io::stdout().flush().expect("flush stdout");

            let mut entry = String::new();
            std::io::stdin()
                .read_line(&mut entry)
                .expect("line read fail");

            if entry.starts_with(':') {
                let pos = entry.find(' ').expect("FIXME");
                let (cmdname, rest) = entry.split_at(pos);
                let cmdname = &cmdname[1..];

                for cmd in &cmds {
                    if cmd.name.starts_with(cmdname) {
                        (cmd.exec)(rest, &mut next_cfg).expect("FIXME");
                    }
                }
                continue 'oneprop;
            } else {
                let result = continue_parse(&entry);
                tracing::debug!(?result, "parse result");
                match result {
                    Some(v) => {
                        outs.push(v);
                        continue 'props;
                    }
                    None => continue 'oneprop,
                }
            }
        }
    }

    (outs, next_cfg)
}

#[tokio::main]
async fn async_main(args: Args) -> Result<()> {
    let api = NotionApi::new(get_config_item("token").await?)?;
    match args.action {
        Action::List => {
            // FIXME: paginate
            let dbs = api
                .search(NotionSearch::filter_by_databases())
                .await?
                .only_databases();

            for db in dbs.results() {
                println!("{}: {}", db.title_plain_text(), db.id);
            }
        }
        Action::Add => {
            let db_id = get_config_item("database").await?;
            let db_id = DatabaseId::from_str(&db_id)?;
            let db = api.get_database(&db_id).await?;
            tracing::debug!(props = field::debug(&db.properties), "db props");

            let read_config = get_config_item("properties.json")
                .await
                .and_then(|content| serde_json::from_str(&content).note(""))
                .unwrap_or_default();

            let props = db.properties.clone();
            let (new_values, new_config) =
                tokio::task::spawn_blocking(|| get_props_from_user(props, read_config)).await?;

            tracing::debug!(?new_values, ?new_config, "new values from user");

            api.create_page(&Parent::Database { database_id: db_id }, &new_values, &[])
                .await?;
        }
    }
    Ok(())
}

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    action: Action,
}

#[derive(clap::Subcommand)]
enum Action {
    List,
    Add,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::try_init()
        .expect("FIXME: report tracing subscriber init failure better");
    let args = Args::parse();
    async_main(args)?;

    Ok(())
}
