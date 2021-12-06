#![allow(
    clippy::non_ascii_literal, // kurwa!
    clippy::too_many_arguments, // uhh great, but how tf do you expect me to do it otherwise?
    clippy::unreadable_literal // it's not unreadable, it's a fucking hex color
)]

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
    gateway::payload::incoming::{MessageCreate, ReactionAdd},
    id::{ChannelId, UserId},
};
use twilight_standby::Standby;

const CIRCLE_EMOJIS: &[&str] =
    &["\u{1f535}", "\u{1f7e1}", "\u{1f7e2}", "\u{1f534}", "\u{26aa}", "\u{1f7e0}", "\u{1f7e4}", "\u{1f7e3}"];
const YESNO_EMOJIS: &[&str] = &["\u{1f975}", "\u{1f976}"];

lazy_static! {
    static ref HELP_EMBED: [Embed; 1] = [Embed {
        author: None,
        color: Some(0x78deb8),
        description: Some(
            concat!(
                "Dostępne komendy:\n\n",
                "\u{2800}- Pomoc\n",
                "\u{2800}\u{2800}\u{30fb} `lbot help`\n",
                "\u{2800}- Głosowanie\n",
                "\u{2800}\u{2800}\u{30fb} `lbot wybory <id kanału> <czas> <wzmianki...>`\n",
                "\u{2800}\u{2800}\u{30fb} `lbot referendum <id kanału> <czas>`\n"
            )
            .to_string()
        ),
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
    let owner = env::var("LKXV_OWNER")?.parse()?;
    let scheme = ShardScheme::Auto;
    let (cluster, mut events) = Cluster::builder(
        token.clone(),
        Intents::GUILD_MESSAGES | Intents::GUILD_MESSAGE_REACTIONS,
    )
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
            if let Err(e) = handle_event(event, http, standby, owner).await {
                tracing::error!("Error in the event handler: {}", e);
            }
        });
    }

    Ok(())
}

async fn execute_poll<'a>(
    http: &Client,
    standby: Arc<Standby>,
    original_channel: ChannelId,
    authorid: UserId,
    channel: &str,
    duration: &str,
    timestamp: Timestamp,
    options: &[&str],
    reactions: &[&str],
    selfvote_block: bool, // if 'options' contains mentions, this will prevent voting for your own mention
) -> anyhow::Result<()> {
    let duration = if let Some(o) = duration_from_shortened(duration) {
        o
    } else {
        http.create_message(original_channel).content("**Błąd:** Niepoprawny format czasu.")?.exec().await?;
        return Ok(());
    };
    let end_timestamp = timestamp.as_secs() as u64 + duration;
    let channel_id = if let Some(o) = channel.parse::<u64>().ok().and_then(ChannelId::new) {
        o
    } else {
        http.create_message(original_channel).content("**Błąd:** Niepoprawne ID kanału.")?.exec().await?;
        return Ok(());
    };
    http.create_message(original_channel).content("Podaj tytuł głosowania [lub 'anuluj']:")?.exec().await?;
    let title = standby
        .wait_for_message(original_channel, move |v: &MessageCreate| v.author.id == authorid)
        .await?
        .content
        .clone();
    if title == "anuluj" {
        http.create_message(original_channel).content("Anulowano.")?.exec().await?;
        return Ok(());
    }
    http.create_message(original_channel).content("Podaj opis głosowania [lub 'brak', 'anuluj']:")?.exec().await?;
    let desc = standby
        .wait_for_message(original_channel, move |v: &MessageCreate| v.author.id == authorid)
        .await?
        .content
        .clone();
    if desc == "anuluj" {
        http.create_message(original_channel).content("Anulowano.")?.exec().await?;
        return Ok(());
    }
    http.create_message(original_channel).content("Tworzenie głosowania...")?.exec().await?;
    let mut embed_description = String::new();
    if desc != "brak" {
        embed_description.push_str(desc.as_str());
        embed_description.push('\n');
    }
    let mut votes: HashMap<&str, (&str, u32)> = HashMap::new();
    for option in options.iter().zip(reactions) {
        write!(&mut embed_description, "\n\u{2800}{} \u{30fb} {}", option.1, option.0)?;
        votes.insert(option.1, (option.0, 0));
    }
    write!(&mut embed_description, "\n\nGłosowanie zakończy się <t:{}:R>", end_timestamp)?;
    let poll_embed = Embed {
        author: None,
        color: Some(0x78deb8),
        title: Some(title),
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
    let poll_msg = http.create_message(channel_id).embeds(&[poll_embed])?.exec().await?.model().await?;

    for reaction in votes.keys() {
        http.create_reaction(poll_msg.channel_id, poll_msg.id, &RequestReactionType::Unicode { name: reaction })
            .exec()
            .await?;
    }
    let mut reaction_stream = standby.wait_for_reaction_stream(poll_msg.id, move |v: &ReactionAdd| v.user_id != poll_msg.author.id);
    let collector = async {
        http.create_message(original_channel).content("Głosowanie zostało utworzone!")?.exec().await?;
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
                        send_dm(http, reaction.user_id, "**Błąd:** Nie możesz głosować na siebie.").await?;
                        continue;
                    }
                    *count += 1;
                    send_dm(http, reaction.user_id, format!("Twój głos został ustawiony na '{}'.", option).as_str())
                        .await?;
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
    let vote_count = votes.values().fold(0_u32, |a, b| a + b.1);
    let mut embed_description = String::new();
    if vote_count > 0 {
        write!(&mut embed_description, "Ilość głosów: {}\n\n", vote_count)?;
        let mut sorted_values: Vec<_> = votes.values().collect();
        sorted_values.sort_by(|a, b| b.1.cmp(&a.1));
        for entry in sorted_values {
            writeln!(&mut embed_description, "\u{2800}\u{30fb} {}: {}% ({} gł.)", entry.0, entry.1 * 100 / vote_count, entry.1)?;
        }
    } else {
        embed_description.push_str("Brak głosów.");
    }
    let results_embed = Embed {
        author: None,
        color: Some(0x78deb8),
        title: Some("Głosowanie zakończone!".to_string()),
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
    http.create_message(channel_id).embeds(&[results_embed])?.exec().await?.model().await?;
    Ok(())
}

fn rtype_to_reqrtype(input: &ReactionType) -> RequestReactionType {
    match input {
        ReactionType::Custom { animated: _, id, name } => {
            RequestReactionType::Custom { id: *id, name: name.as_ref().map(String::as_str) }
        }
        ReactionType::Unicode { name } => RequestReactionType::Unicode { name: name.as_str() },
    }
}

async fn send_dm(http: &Client, with: UserId, content: &str) -> anyhow::Result<()> {
    let dm = http.create_private_channel(with).exec().await?.model().await?.id;
    http.create_message(dm).content(content)?.exec().await?;
    Ok(())
}

async fn handle_message(msg: &MessageCreate, http: &Client, standby: Arc<Standby>, owner: u64) -> anyhow::Result<()> {
    if !msg.content.starts_with("lbot ") {
        return Ok(());
    }
    let split: Vec<&str> = msg.content.split_whitespace().skip(1).collect();
    match split.get(0) {
        Some(&"help") => {
            http.create_message(msg.channel_id).embeds(&*HELP_EMBED)?.exec().await?;
        }
        Some(&"wybory") if split.len() > 2 => {
            if msg.author.id.get() != owner {
                http.create_message(msg.channel_id).content("**Błąd:** Niewystarczające uprawnienia.")?.exec().await?;
                return Ok(());
            }
            let mention_count = msg.mentions.len();
            if split.len() - mention_count != 3 {
                http.create_message(msg.channel_id).content("**Błąd:** Nieprawidłowe użycie.")?.exec().await?;
                return Ok(());
            }
            if !(2..=8).contains(&mention_count) {
                http.create_message(msg.channel_id)
                    .content("**Błąd:** Liczba kandydatów musi mieścić się w przedziale od 2 do 8.")?
                    .exec()
                    .await?;
                return Ok(());
            }
            execute_poll(
                http,
                standby,
                msg.channel_id,
                msg.author.id,
                split[1],
                split[2],
                msg.timestamp,
                &split[3..],
                CIRCLE_EMOJIS,
                true,
            )
            .await?;
        }
        Some(&"referendum") if split.len() == 3 => {
            if msg.author.id.get() != owner {
                http.create_message(msg.channel_id).content("**Błąd:** Niewystarczające uprawnienia.")?.exec().await?;
                return Ok(());
            }
            execute_poll(
                http,
                standby,
                msg.channel_id,
                msg.author.id,
                split[1],
                split[2],
                msg.timestamp,
                &["Za", "Przeciw"],
                YESNO_EMOJIS,
                false,
            )
            .await?;
        }
        _ => {
            http.create_message(msg.channel_id)
                .content("**Błąd:** Nieznana komenda lub niepoprawne użycie.")?
                .exec()
                .await?;
        }
    }
    Ok(())
}

async fn handle_event(event: Event, http: Arc<Client>, standby: Arc<Standby>, owner: u64) -> anyhow::Result<()> {
    if let Event::MessageCreate(msg) = event {
        if let Err(e) = handle_message(&msg, &http, standby, owner).await {
            http.create_message(msg.channel_id).content(format!("**Wyjątek:** {}", e).as_str())?.exec().await?;
            return Err(e);
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
