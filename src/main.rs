#![feature(iter_intersperse)]
mod util;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{SeekFrom, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;

use clap::StructOpt;
use color_eyre::Result;
use color_eyre::{
    eyre::{eyre, Context},
    Help,
};
use date_time_parser::DateParser;
use notion::ids::{AsIdentifier, DatabaseId, PageId, PropertyId};
use notion::models::properties::{
    Color as NotionColor, DateOrDateTime, DateValue, PropertyConfiguration, PropertyValue,
    Relation, RelationValue,
};
use notion::models::search::DatabaseQuery;
use notion::models::text::RichText;
use notion::models::Parent;
use notion::{models::search::NotionSearch, NotionApi};
use owo_colors::colors::css;
use owo_colors::{OwoColorize, Style};
use serde::{Deserialize, Serialize};
use skim::prelude::SkimOptionsBuilder;
use skim::{Skim, SkimItem, SkimItemReceiver, SkimOptions};
use tokio::fs::{read_to_string, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::field;

use crate::util::IndentedText;

fn path_for_config_item(name: &str) -> Result<PathBuf> {
    let mut path = dirs::config_dir().ok_or(eyre!("couldn't get config dir"))?;
    path.push("notion-entry");
    path.push(name);
    Ok(path)
}

async fn get_config_item(name: &str) -> Result<String> {
    let path = path_for_config_item(name)?;
    read_to_string(&path)
        .await
        .context("reading config")
        .with_suggestion(|| format!("consider creating {:?}", &path))
        .map(|item| item.trim().to_string())
}

type Props = HashMap<String, PropertyConfiguration>;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct PropertyReadConfig {
    order: Vec<PropertyId>,
    default_values: HashMap<PropertyId, PropertyValue>,
}

#[derive(Clone, Debug)]
struct RelationItem {
    title: String,
    page_id: PageId,
}

impl SkimItem for RelationItem {
    fn text(&self) -> Cow<str> {
        Cow::Borrowed(&self.title)
    }
}

struct RelationCache {
    db_name: String,
    items: Arc<Vec<RelationItem>>,
}

impl RelationCache {
    async fn new(api: &NotionApi, db_id: &DatabaseId) -> Result<RelationCache> {
        // FIXME: pagination
        // FIXME: this should really probably do it incrementally on keystroke
        // or even populate live rather than running at the start but i cannot
        // deal with writing that right now
        let db = api.get_database(db_id).await?;
        let db_name = db.title.iter().map(|rt| rt.plain_text()).collect();

        let results = api.query_database(db_id, DatabaseQuery::default()).await?;
        let items = Arc::new(
            results
                .results
                .into_iter()
                .filter_map(|result| {
                    Some(RelationItem {
                        title: result.title()?,
                        page_id: result.id,
                    })
                })
                .collect(),
        );

        Ok(RelationCache { items, db_name })
    }

    fn as_skim(&self) -> SkimItemReceiver {
        let (skim_s, skim_r) = crossbeam_channel::unbounded();

        let items = self.items.clone();
        thread::spawn(move || {
            for item in &*items {
                let item: Arc<dyn SkimItem> = Arc::new(item.clone());
                if let Err(_) = skim_s.send(item) {
                    // the skim finished before us, just leave
                    break;
                }
            }
        });

        skim_r
    }
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
type SkimToValue = Box<dyn FnOnce(Vec<Arc<dyn SkimItem>>) -> Option<PropertyValue>>;

enum PromptType {
    Text(String, ParseContinuation),
    Skim(String, SkimOptions<'static>, SkimItemReceiver, SkimToValue),
}

fn prompt_for(
    name: &str,
    cfg: PropertyConfiguration,
    relation_caches: &HashMap<DatabaseId, RelationCache>,
) -> Option<PromptType> {
    Some(match cfg {
        PropertyConfiguration::Text { id } => PromptType::Text(
            format!("{name}:"),
            Box::new(move |input| {
                Some(PropertyValue::Text {
                    id,
                    rich_text: vec![RichText::from_plain_text(input)],
                })
            }),
        ),
        PropertyConfiguration::Title { id } => PromptType::Text(
            format!("{name}:"),
            Box::new(move |input| {
                Some(PropertyValue::Title {
                    id,
                    title: vec![RichText::from_plain_text(input)],
                })
            }),
        ),
        PropertyConfiguration::Select { id, select } => PromptType::Text(
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
        PropertyConfiguration::Date { id } => PromptType::Text(
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
        PropertyConfiguration::Url { id } => PromptType::Text(
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
        PropertyConfiguration::Relation {
            id,
            relation: Relation { database_id, .. },
        } => {
            let rc = relation_caches.get(&database_id)?;
            let prompt = rc.db_name.clone();
            let skim_options = SkimOptionsBuilder::default().multi(true).build().unwrap();
            PromptType::Skim(
                prompt,
                skim_options,
                rc.as_skim(),
                Box::new(|items| {
                    let values = items
                        .into_iter()
                        .filter_map(|item| {
                            let page: Option<&RelationItem> = item.as_any().downcast_ref();
                            tracing::debug!(?page, "relation item");
                            let page = page?;
                            Some(RelationValue {
                                id: page.page_id.clone(),
                            })
                        })
                        .collect();

                    tracing::debug!(?values, "user selected relation items");

                    Some(PropertyValue::Relation {
                        id,
                        relation: Some(values),
                    })
                }),
            )
        }
        _ => return None,
    })
}

struct PropEntryCommandDef {
    name: &'static str,
    usage: &'static str,
    exec: fn(&str, &mut PropertyReadConfig, &PropertyConfiguration) -> Result<()>,
}

static PROP_ENTRY_COMMANDS: &[PropEntryCommandDef] = &[PropEntryCommandDef {
    name: "order",
    usage: ":order N\nwhere N is the 0-indexed position it should be in",
    exec: (|args, cf, prop| {
        let new_pos = args.trim().parse::<usize>()?.min(cf.order.len() - 1);
        let prop_id = prop.as_id();
        let current_pos = cf.order.iter().position(|v| v == prop_id);
        if let Some(current_pos) = current_pos {
            cf.order.remove(current_pos);
        }

        cf.order.insert(new_pos, prop_id.clone());
        Ok(())
    }),
}];

enum PromptResult {
    TryAgain,
    NextProp(Option<PropertyValue>),
}

fn text_prompt(
    prompt: &str,
    continue_parse: ParseContinuation,
    prop_def: &PropertyConfiguration,
    next_cfg: &mut PropertyReadConfig,
) -> PromptResult {
    println!("{}", prompt);
    print!("> ");
    std::io::stdout().flush().expect("flush stdout");

    let mut entry = String::new();
    std::io::stdin()
        .read_line(&mut entry)
        .expect("line read fail");

    if entry.starts_with(':') {
        let pos = entry.find(' ').unwrap_or(entry.len());
        let (cmdname, rest) = entry.split_at(pos);
        let cmdname = &cmdname[1..];

        for cmd in PROP_ENTRY_COMMANDS {
            if cmd.name.starts_with(cmdname) {
                let result = (cmd.exec)(rest, next_cfg, &prop_def);
                if let Err(err) = result {
                    println!("Error: {}", err);
                    println!("Usage:\n{}", IndentedText(4, cmd.usage));
                }
                return PromptResult::TryAgain;
            }
        }

        println!("Unknown command. Supported commands:");
        for cmd in PROP_ENTRY_COMMANDS {
            println!("{}:\n{}", cmd.name, IndentedText(4, cmd.usage));
        }
        println!();

        PromptResult::TryAgain
    } else {
        let result = continue_parse(&entry.trim_end());
        tracing::debug!(?result, "parse result");
        match result {
            Some(v) => PromptResult::NextProp(Some(v)),
            None => PromptResult::TryAgain,
        }
    }
}

fn skim_prompt<'a>(
    prompt: &'a str,
    mut skim_options: SkimOptions<'a>,
    skim_channel: SkimItemReceiver,
    to_value: SkimToValue,
) -> PromptResult {
    skim_options.prompt = Some(prompt);

    let result = Skim::run_with(&skim_options, Some(skim_channel));

    match result {
        Some(v) => {
            // FIXME: this could be implemented to be able to
            // create pages in the target database somehow.
            // would have to figure out how to get skim to do
            // this for us more cleverly though!
            //
            // Also, we do not handle aborts, which we should.
            tracing::debug!(?v.query, ?v.cmd, ?v.final_event, "skim returns");
            PromptResult::NextProp(to_value(v.selected_items))
        }
        None => {
            tracing::debug!("skim returned nothing");
            PromptResult::NextProp(None)
        }
    }
}

fn get_props_from_user(
    props: Props,
    mut cfg: PropertyReadConfig,
    relation_caches: &HashMap<DatabaseId, RelationCache>,
) -> (Vec<PropertyValue>, PropertyReadConfig) {
    let have_props = cfg.order.iter().cloned().collect::<HashSet<_>>();
    for prop in props.values() {
        if !have_props.contains(prop.as_id()) {
            cfg.order.push(prop.as_id().clone());
        }
    }

    // order now has all of the props
    let mut next_cfg = cfg.clone();

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

            let prompt = prompt_for(name.as_str(), prop_def.clone(), relation_caches);
            let prompt = match prompt {
                Some(v) => v,
                None => {
                    tracing::debug!(?prop_def, "prop type unsupported");
                    continue 'props;
                }
            };

            match match prompt {
                PromptType::Text(prompt, continue_parse) => {
                    text_prompt(&prompt, continue_parse, &prop_def, &mut next_cfg)
                }
                PromptType::Skim(prompt, skim_options, skim_channel, to_value) => {
                    skim_prompt(&prompt, skim_options, skim_channel, to_value)
                }
            } {
                PromptResult::TryAgain => continue 'oneprop,
                PromptResult::NextProp(v) => {
                    if let Some(v) = v {
                        outs.push(v);
                    }
                    continue 'props;
                }
            };
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
            // FIXME: this should be configurable at runtime
            // somehow/selectable?

            let db_id = get_config_item("database").await?;
            let db_id = DatabaseId::from_str(&db_id)?;
            let db = api.get_database(&db_id).await?;
            tracing::debug!(props = field::debug(&db.properties), "db props");

            let mut read_config: PropertyReadConfig = get_config_item("properties.json")
                .await
                .and_then(|content| serde_json::from_str(&content).note(""))
                .unwrap_or_default();

            let mut relation_caches = HashMap::new();
            for prop in db.properties.values() {
                if let PropertyConfiguration::Relation { relation, .. } = prop {
                    if !relation_caches.contains_key(&relation.database_id) {
                        // FIXME: we can do this in parallel for all relations
                        relation_caches.insert(
                            relation.database_id.clone(),
                            RelationCache::new(&api, &relation.database_id).await?,
                        );
                    }
                }
            }

            let mut properties_file = OpenOptions::new()
                .write(true)
                .create(true)
                .open(path_for_config_item("properties.json")?)
                .await?;
            let relation_caches = Arc::new(relation_caches);

            loop {
                let props = db.properties.clone();
                let caches = relation_caches.clone();
                let this_run_read_config = read_config.clone();

                let (new_values, new_config) = tokio::task::spawn_blocking(move || {
                    get_props_from_user(props, this_run_read_config, &caches)
                })
                .await?;

                tracing::debug!(?new_values, ?new_config, "new values from user");

                properties_file.set_len(0).await?;
                properties_file.seek(SeekFrom::Start(0)).await?;
                properties_file
                    .write_all(&serde_json::to_vec(&new_config)?)
                    .await?;
                properties_file.sync_data().await?;
                read_config = new_config;

                let page = api
                    .create_page(
                        &Parent::Database {
                            database_id: db_id.clone(),
                        },
                        &new_values,
                        &[],
                    )
                    .await?;

                println!("Page created: {}\n", page.url);
            }
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
