// exceptions for when `clippy::pedantic` is *too* pedantic
#![allow(
    clippy::non_ascii_literal, // kurwa!
    clippy::too_many_arguments, // uhh great, but how tf do you expect me to do it otherwise?
    clippy::unreadable_literal, // it's not unreadable, it's a hex color
    clippy::too_many_lines, // YOU FORMATTED IT
    clippy::match_on_vec_items // it won't panic, because I tested the length right before the match
)]

use std::{collections::HashMap, env, fmt::Display, sync::Arc, time::Duration};

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
const CANCEL_EMOJI: &str = "\u{274c}";

lazy_static! {
    static ref HELP_EMBED: [Embed; 1] = [Embed {
        author: None,
        color: Some(0x78deb8),
        description: Some(
            concat!(
                "Dostępne komendy:\n\n",
                "\u{2800}\u{25AB} Pomoc\n",
                "\u{2800}\u{2800}\u{30fb} `lbot help`\n",
                "\u{2800}\u{25AB} Głosowanie\n",
                "\u{2800}\u{2800}\u{30fb} `lbot wybory <id kanału> <czas> <wzmianki...>`\n",
                "\u{2800}\u{2800}\u{30fb} `lbot referendum <id kanału> <czas> <ankieta|tn>`"
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
    pretty_env_logger::init_custom_env("LKXV_LOG");
    let token = env::var("LKXV_TOKEN")?;
    let owner = env::var("LKXV_OWNER")?.parse()?;
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
            if let Err(e) = handle_event(event, http, standby, owner).await {
                log::error!("Error in the event handler: {}", e);
            }
        });
    }

    Ok(())
}

async fn execute_poll<'a, T: AsRef<str> + Display + PartialEq<String>>(
    http: &Client,
    standby: Arc<Standby>,
    original_channel: ChannelId,
    authorid: UserId,
    channel: &str,
    duration: &str,
    timestamp: Timestamp,
    options: &[T],
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
    let title =
        &standby.wait_for_message(original_channel, move |v: &MessageCreate| v.author.id == authorid).await?.content;
    if title == "anuluj" {
        http.create_message(original_channel).content("Anulowano.")?.exec().await?;
        return Ok(());
    }
    http.create_message(original_channel).content("Podaj opis głosowania [lub 'anuluj']:")?.exec().await?;
    let desc =
        &standby.wait_for_message(original_channel, move |v: &MessageCreate| v.author.id == authorid).await?.content;
    if desc == "anuluj" {
        http.create_message(original_channel).content("Anulowano.")?.exec().await?;
        return Ok(());
    }
    http.create_message(original_channel).content("Tworzenie głosowania...")?.exec().await?;
    let mut embed_description = format!("Opis:\n{}\n", desc);
    let mut votes: HashMap<UserId, usize> = HashMap::new();
    for option in options.iter().zip(reactions) {
        write!(&mut embed_description, "\n\u{2800}{} \u{30fb} {}", option.1, option.0)?;
    }
    write!(&mut embed_description, "\n\nGłosowanie zakończy się <t:{}:R>", end_timestamp)?;
    let poll_embed = Embed {
        author: None,
        color: Some(0x78deb8),
        title: Some(title.clone()),
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

    for reaction in reactions.iter().take(options.len()).chain(&[CANCEL_EMOJI]) {
        http.create_reaction(poll_msg.channel_id, poll_msg.id, &RequestReactionType::Unicode { name: reaction })
            .exec()
            .await?;
    }
    let mut reaction_stream =
        standby.wait_for_reaction_stream(poll_msg.id, move |v: &ReactionAdd| v.user_id != poll_msg.author.id);
    let collector = async {
        http.create_message(original_channel).content("Głosowanie zostało utworzone!")?.exec().await?;
        log::info!("Poll '{}' has been created, listening for reactions", title);
        while let Some(reaction) = reaction_stream.next().await {
            http.delete_reaction(
                reaction.channel_id,
                reaction.message_id,
                &rtype_to_reqrtype(&reaction.emoji),
                reaction.user_id,
            )
            .exec()
            .await?;
            if let ReactionType::Unicode { name } = &reaction.emoji {
                if let Some(o) = reactions.iter().take(options.len()).position(|&v| v == name) {
                    if selfvote_block && options[o] == format!("<@!{}>", reaction.user_id) {
                        log::info!("User {} tried to vote for themselves!", reaction.user_id);
                        let _ = send_dm(http, reaction.user_id, "**Błąd:** Nie możesz głosować na siebie.").await;
                        continue;
                    }
                    log::info!("User {} voted for {}", reaction.user_id, options[o]);
                    let _ = send_dm(
                        http,
                        reaction.user_id,
                        if let Some(v) = votes.insert(reaction.user_id, o) {
                            format!("Twój głos został zmieniony z '{}' na '{}'.", options[v], options[o])
                        } else {
                            format!("Twój głos został ustawiony na '{}'.", options[o])
                        }
                        .as_str(),
                    )
                    .await;
                } else if name == CANCEL_EMOJI {
                    if reaction.user_id == authorid {
                        return Ok(())
                    } else {
                        let _ = send_dm(http, reaction.user_id, "**Błąd:** Niewystarczające uprawnienia.").await;
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    select! {
        r = collector => {r?},
        _ = sleep(Duration::from_secs(duration)) => {}
    };
    let mut vote_count_full = 0;
    let mut vote_counts = vec![0_usize; options.len()];
    let mut embed_description = format!("Opis:\n{}\n\n", desc);
    for vote in votes {
        vote_count_full += 1;
        vote_counts[vote.1] += 1;
    }
    log::info!("Poll {} finished, {} votes", title, vote_count_full);
    if vote_count_full > 0 {
        write!(&mut embed_description, "Ilość głosów: {}\n\n", vote_count_full)?;
        vote_counts.sort_by(|a, b| b.cmp(a));
        for vote_count in vote_counts.iter().enumerate() {
            let option = &options[vote_count.0];
            let percent = vote_count.1 * 100 / vote_count_full;
            let votes = vote_count.1;
            log::info!("{}: {}% ({} votes)", option, percent, votes);
            writeln!(&mut embed_description, "\u{2800}\u{30fb} {}: {}% ({} gł.)", option, percent, votes)?;
        }
    } else {
        embed_description.push_str("Brak głosów.");
    }
    let results_embed = Embed {
        author: None,
        color: Some(0x78deb8),
        title: Some(title.clone()),
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
    http.delete_all_reactions(poll_msg.channel_id, poll_msg.id).exec().await?;
    http.update_message(poll_msg.channel_id, poll_msg.id).embeds(&[results_embed])?.exec().await?;
    http.create_message(poll_msg.channel_id)
        .content("Głosowanie zostało zakończone!")?
        .reply(poll_msg.id)
        .exec()
        .await?;
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
    if split.is_empty() {
        return Ok(());
    }
    match split[0] {
        "help" => {
            http.create_message(msg.channel_id).embeds(&*HELP_EMBED)?.exec().await?;
        }
        "wybory" if split.len() > 2 => {
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
        "referendum" if split.len() == 4 => {
            let authorid = msg.author.id;
            if authorid.get() != owner {
                http.create_message(msg.channel_id).content("**Błąd:** Niewystarczające uprawnienia.")?.exec().await?;
                return Ok(());
            }
            match split[3] {
                "tn" => {
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
                "ankieta" => {
                    let mut options: Vec<String> = Vec::with_capacity(8);
                    let mut option_stream = standby
                        .wait_for_message_stream(msg.channel_id, move |v: &MessageCreate| v.author.id == authorid)
                        .take(8);
                    http.create_message(msg.channel_id)
                        .content("Podaj 8 opcji (lub mniej z pomocą 'koniec')")?
                        .exec()
                        .await?;
                    while let Some(opt) = option_stream.next().await {
                        if opt.content == "koniec" {
                            break;
                        }
                        options.push(opt.content.clone());
                    }
                    if options.is_empty() {
                        http.create_message(msg.channel_id)
                            .content("**Błąd:** Nie podano żadnych opcji.")?
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
                        &options,
                        CIRCLE_EMOJIS,
                        false,
                    )
                    .await?;
                }
                _ => {
                    http.create_message(msg.channel_id)
                        .content("**Błąd:** Nieprawidłowy typ referendum (użyj 'tn' lub 'ankieta').")?
                        .exec()
                        .await?;
                }
            }
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
    let suffix = shortened.chars().last()?;
    shortened[..shortened.len() - 1].parse::<u64>().ok().and_then(|v| match suffix {
        's' => Some(v),
        'm' => Some(v * 60),
        'h' => Some(v * 3600),
        'd' => Some(v * 86400),
        _ => None,
    })
}
