#![allow(clippy::non_ascii_literal)] // kurwa!

use std::{collections::HashMap, env, sync::Arc, time::Duration};

use futures_util::StreamExt;
use lazy_static::lazy_static;
use std::fmt::Write;
use tokio::select;
use tokio::time::sleep;
use twilight_gateway::{cluster::ShardScheme, Cluster, Event, Intents};
use twilight_http::{request::channel::reaction::RequestReactionType, Client};
use twilight_model::{
    channel::{embed::Embed, ReactionType},
    datetime::Timestamp,
    gateway::payload::incoming::ReactionAdd,
    id::ChannelId,
};
use twilight_standby::Standby;

const REACTION_EMOJIS: &[&str] =
    &["\u{1f535}", "\u{1f7e1}", "\u{1f7e2}", "\u{1f534}", "\u{26aa}", "\u{1f7e0}", "\u{1f7e4}", "\u{1f7e3}"];

lazy_static! {
    static ref HELP_EMBED: [Embed; 1] = [Embed {
        author: None,
        color: Some(0x78deb8),
        description: Some(concat!(
            "Dostępne komendy:\n\n",
            "\u{2800}\u{2022} `lbot help` - \n",
            "\u{2800}\u{2022} `lbot wybory` - "
        ).to_string()),
        fields: vec![],
        footer: None,
        image: None,
        kind: "rich".to_string(),
        provider: None,
        thumbnail: None,
        timestamp: None,
        title: Some("LKXVbot".to_string()),
        url: None,
        video: None,
    }];
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let token = env::var("LKXV_TOKEN")?;
    let scheme = ShardScheme::Auto;
    let (cluster, mut events) =
        Cluster::builder(token.clone(), Intents::GUILD_MESSAGES | Intents::GUILD_MESSAGE_REACTIONS)
            .shard_scheme(scheme)
            .build()
            .await?;
    let cluster = Arc::new(cluster);
    let cluster_spawn = Arc::clone(&cluster);
    tokio::spawn(async move {
        cluster_spawn.up().await;
    });
    let http = Arc::new(Client::new(token));
    let standby = Arc::new(Standby::new());

    while let Some((_, event)) = events.next().await {
        standby.process(&event);
        let http = Arc::clone(&http);
        let standby = Arc::clone(&standby);
        tokio::spawn(async move {
            if let Err(e) = handle_event(event, http, standby).await {
                tracing::error!("Error in the event handler: {}", e);
            }
        });
    }

    Ok(())
}

async fn create_poll<'a>(
    http: Arc<Client>,
    standby: Arc<Standby>,
    channel: ChannelId,   // where poll will happen
    timestamp: Timestamp, // start of the poll
    duration: u64,
    name: String,
    options: &'a [&str],
    selfvote_block: bool, // if 'options' contains mentions, this will prevent voting for your own mention
) -> anyhow::Result<HashMap<&'static str, (&'a str, u32)>> {
    let end_timestamp = timestamp.as_secs() as u64 + duration;
    let mut embed_description = format!("Głosowanie zakończy się <t:{}:R>\n\n", end_timestamp);
    let mut votes: HashMap<&str, (&str, u32)> = HashMap::new();
    for option in options.iter().zip(REACTION_EMOJIS) {
        writeln!(&mut embed_description, "\u{2800}{} \u{2022} {}", option.1, option.0)?;
        votes.insert(option.1, (option.0, 0));
    }
    let poll_embed = Embed {
        author: None,
        color: Some(0x78deb8),
        title: Some(name),
        description: Some(embed_description),
        fields: vec![],
        kind: "rich".to_string(),
        footer: None,
        image: None,
        provider: None,
        thumbnail: None,
        timestamp: None,
        url: None,
        video: None,
    };
    let poll_msg = http.create_message(channel).embeds(&[poll_embed])?.exec().await?.model().await?;

    for (reaction, _) in &votes {
        http.create_reaction(poll_msg.channel_id, poll_msg.id, &RequestReactionType::Unicode { name: reaction })
            .exec()
            .await?;
    }
    // why would you EVER need a predicate there?!?!!?
    // ...we'll just return true for everything
    let mut reaction_stream = standby.wait_for_reaction_stream(poll_msg.id, |_: &ReactionAdd| true);
    let collector = async {
        while let Some(reaction) = reaction_stream.next().await {
            if let ReactionType::Unicode { name } = &reaction.emoji {
                if let Some((option, count)) = votes.get_mut(name.as_str()) {
                    if selfvote_block && *option == format!("<@!{}>", reaction.user_id) {
                        http.delete_reaction(
                            reaction.channel_id,
                            reaction.message_id,
                            &rtype_to_reqrtype(&reaction.emoji),
                            reaction.user_id,
                        )
                        .exec()
                        .await?;
                        continue;
                    }
                    *count += 1;
                }
            }
            http.delete_reaction(
                reaction.channel_id,
                reaction.message_id,
                &rtype_to_reqrtype(&reaction.emoji),
                reaction.user_id,
            )
            .exec()
            .await?;
        }
        anyhow::Ok(())
    };
    select! {
        r = collector => {r?},
        _ = sleep(Duration::from_secs(duration)) => {}
    };
    http.delete_all_reactions(poll_msg.channel_id, poll_msg.id).exec().await?;
    Ok(votes)
}

fn rtype_to_reqrtype<'a>(input: &'a ReactionType) -> RequestReactionType<'a> {
    match input {
        ReactionType::Custom { animated: _, id, name } => {
            RequestReactionType::Custom { id: *id, name: name.as_ref().map(String::as_str) }
        }
        ReactionType::Unicode { name } => RequestReactionType::Unicode { name: name.as_str() },
    }
}

async fn handle_event(event: Event, http: Arc<Client>, standby: Arc<Standby>) -> anyhow::Result<()> {
    if let Event::MessageCreate(msg) = event {
        if !msg.content.starts_with("lbot ") {
            return Ok(());
        }
        let split: Vec<&str> = msg.content.split_whitespace().skip(1).collect();
        match split.get(0) {
            Some(&"help") => {
                http.create_message(msg.channel_id).embeds(&*HELP_EMBED)?.exec().await?;
            }
            Some(&"wybory") if split.len() > 2 => {
                let mention_count = msg.mentions.len();
                if !(2..=8).contains(&mention_count) {
                    http.create_message(msg.channel_id)
                        .content("**Błąd:** Liczba kandydatów musi mieścić się w przedziale od 2 do 8.")?
                        .exec()
                        .await?;
                    return Ok(());
                }
                let duration = if let Some(o) = duration_from_shortened(split[2]) {
                    o
                } else {
                    http.create_message(msg.channel_id).content("**Błąd:** Niepoprawny format czasu.")?.exec().await?;
                    return Ok(());
                };
                create_poll(
                    http.clone(),
                    standby,
                    msg.channel_id,
                    msg.timestamp,
                    duration,
                    split[1].to_string(),
                    &split[3..],
                    true,
                )
                .await?;
                http.create_message(msg.channel_id).content("Koniec")?.exec().await?;
            }
            _ => {
                http.create_message(msg.channel_id).content("**Błąd:** Nieznana komenda lub niepoprawne użycie.")?.exec().await?;
            }
        }
    }

    Ok(())
}

fn duration_from_shortened(shortened: &str) -> Option<u64> {
    if shortened.len() < 2 {
        return None;
    }
    let suffix = shortened.chars().last().unwrap();
    shortened[..shortened.len() - 1].parse::<u64>().ok().and_then(|v| match suffix {
        's' => Some(v),
        'm' => Some(v * 60),
        'h' => Some(v * 3600),
        'd' => Some(v * 86400),
        _ => None,
    })
}
