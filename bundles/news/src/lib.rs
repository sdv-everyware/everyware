//! NewsServiceBundle: fetches the top stories of Hacker News every ten minutes, the description
//! and the picture of each article (its page's meta tags, once per story), and publishes the
//! feed on the topic `news` and the thumbnails of its stories on `news-thumbnails` (catalog/news.proto,
//! catalog/everyware.vsidl) for the house. The screen gets the feed
//! through the dashboard bundle; nothing else in the house talks to the internet for it.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's publishers; the
//! business logic is in `handle_news_publisher`.

use everyware_bundle_store::Store;
use everyware_catalog::news::{NewsFeed, NewsStory, NewsThumbnail, NewsThumbnails};
use everyware_news_client as client;
use futures::future::join_all;
use log::{debug, error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv_mw_rs_com_everyware_news_service_bundle::{NewsServiceBundle as Bundle, PublisherDescriptors};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// The stories the feed carries.
const STORIES: usize = 10;
/// Hacker News' front page moves within minutes; ten is enough for a panel.
const POLL_INTERVAL: Duration = Duration::from_secs(600);
/// The bundle starts before DHCP has a lease and before chrony has set the clock (the TLS
/// certificate is then "not yet valid"), so the first attempts fail; a failed attempt costs
/// one request, so try again at once.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// The last feed and its thumbnails, published again at the next start until the first new
/// feed.
const LAST_FEED: &str = "feed.pb";
const LAST_THUMBNAILS: &str = "thumbnails.pb";
/// Older than this the last feed is not shown: the front page has moved on.
const LAST_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

sdv::lifecycle::register_service_bundle!(NewsServiceBundle);

struct NewsServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
    /// The bundle's directory (/data/service_bundle_data/<package>/<bundle>).
    store: Arc<Store>,
}

impl ServiceBundle for NewsServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        let fqin = context.get_self_fqin();
        let store = Arc::new(Store::new(fqin.get_sdv_package_name(), fqin.get_service_bundle_name()));
        Self { cancellation_token: None, context, store }
    }

    fn on_start(&mut self) {
        // The runner has set up the logger already, with log_tag from the bundle manifest.
        let context = self.context;
        let store = self.store.clone();
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());
        let Ok(runtime) =
            tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build()
        else {
            panic!("Failed to start the runtime!");
        };
        let _ = std::thread::spawn(move || {
            runtime.block_on(async move {
                let comms = Arc::new(SdvComms { context });
                let res = tokio::select! {
                    res = sdv_main(comms, store) => res,
                    () = cancellation_token.cancelled() => Err(sdv::status::SdvStatus::new(sdv::status::SdvStatusCode::Cancelled)),
                };
                error!("Service bundle loop stopped: {res:?}.");
            });
            drop(runtime);
        });
    }

    fn on_stop(&mut self) {
        self.cancellation_token.as_mut().expect("Must have been created on_start").cancel();
        self.cancellation_token = None;
    }
}

async fn sdv_main(comms: Arc<dyn Communicate>, store: Arc<Store>) -> sdv::status::SdvResult<()> {
    info!("Starting service bundle 'NewsServiceBundle'");
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).build().await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(sdv::status::SdvStatus::with_message(sdv::status::SdvStatusCode::Internal, e.to_string()));
        }
    };
    let publisher = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<NewsFeed>::NEWS).await.map_err(|e| {
        error!("Failed to create publisher NewsFeed#news: {e:?}");
        e
    })?;
    let thumbnails = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<NewsThumbnails>::NEWS_THUMBNAILS).await.map_err(|e| {
        error!("Failed to create publisher NewsThumbnails#news-thumbnails: {e:?}");
        e
    })?;
    handle_news_publisher(publisher, thumbnails, &store).await;
    Ok(())
}

/// What was read of a story's article, kept by id while the story is in the feed.
#[derive(Default)]
struct Article {
    summary: String,
    /// The thumbnail (RGB); None when the page has no picture or it could not be made.
    thumbnail: Option<Vec<u8>>,
}

/// The last feed first, then the top stories every ten minutes. A round publishes the feed as
/// soon as the stories are known (the summaries read before), reads the articles and the
/// pictures of the new stories, then publishes the feed again with their summaries and the
/// thumbnails of the stories that have one, all in one message. A story's article and
/// picture are read once.
async fn handle_news_publisher(publisher: Publisher<NewsFeed>, thumbnails: Publisher<NewsThumbnails>, store: &Store) {
    let mut articles: HashMap<u64, Article> = HashMap::new();
    if let Some(feed) = store.load::<NewsFeed>(LAST_FEED, LAST_MAX_AGE) {
        articles = restore(store, &feed);
        info!("the last feed: {} stories, {} with a picture", feed.stories.len(), articles.values().filter(|a| a.thumbnail.is_some()).count());
        // without its thumbnails the feed goes out with what it has, its articles are read again
        if articles.is_empty() {
            info!("no thumbnails kept, the articles are read again");
        }
        publish(&publisher, &thumbnails, &feed, &articles);
    }
    info!("polling Hacker News every {} s", POLL_INTERVAL.as_secs());
    loop {
        let wait = match round(&publisher, &thumbnails, store, &mut articles).await {
            Ok(()) => POLL_INTERVAL,
            Err(err) => {
                warn!("fetch failed: {err}");
                RETRY_INTERVAL
            }
        };
        debug!("next feed in {} s", wait.as_secs());
        sleep(wait).await;
    }
}

/// One round: the stories, the feed, the new articles, the feed again with them, kept.
async fn round(
    publisher: &Publisher<NewsFeed>,
    thumbnails: &Publisher<NewsThumbnails>,
    store: &Store,
    articles: &mut HashMap<u64, Article>,
) -> client::Result<()> {
    let stories = top_stories().await?;
    let new: Vec<&client::Story> = stories.iter().filter(|s| !s.url.is_empty() && !articles.contains_key(&s.id)).collect();
    if !new.is_empty() {
        let feed = to_message(&stories, articles);
        info!("{} stories, {} new, their articles next", feed.stories.len(), new.len());
        if let Err(e) = publisher.publish(&feed) {
            error!("Failed to publish on NewsFeed#news: {e:?}");
        }
    }
    let read = join_all(new.iter().map(|s| read_article(s))).await;
    for (story, article) in new.iter().zip(read) {
        articles.insert(story.id, article);
    }
    articles.retain(|id, _| stories.iter().any(|s| s.id == *id));
    let feed = to_message(&stories, articles);
    info!(
        "{} stories, {} with a summary, {} with a picture",
        feed.stories.len(),
        feed.stories.iter().filter(|s| !s.summary.is_empty()).count(),
        articles.values().filter(|a| a.thumbnail.is_some()).count()
    );
    if !publish(publisher, thumbnails, &feed, articles) {
        return Err("the feed was not published".into());
    }
    keep(store, &feed, articles);
    Ok(())
}

/// The top stories (more ids than stories are read: some items are not live stories, dead or
/// a job).
async fn top_stories() -> client::Result<Vec<client::Story>> {
    let ids = client::top_ids().await?;
    let items = join_all(ids.iter().take(STORIES + 5).map(|&id| client::story(id))).await;
    let stories: Vec<client::Story> = items.into_iter().filter_map(|r| r.ok().flatten()).take(STORIES).collect();
    if stories.is_empty() {
        return Err("no stories".into());
    }
    Ok(stories)
}

/// The feed, then the thumbnails of its stories; false when the feed could not be published.
fn publish(publisher: &Publisher<NewsFeed>, thumbnails: &Publisher<NewsThumbnails>, feed: &NewsFeed, articles: &HashMap<u64, Article>) -> bool {
    if let Err(e) = publisher.publish(feed) {
        error!("Failed to publish on NewsFeed#news: {e:?}");
        return false;
    }
    if let Err(e) = thumbnails.publish(&pictures(feed, articles)) {
        error!("Failed to publish on NewsThumbnails#news-thumbnails: {e:?}");
    }
    true
}

/// The thumbnails of the stories of `feed` that have one.
fn pictures(feed: &NewsFeed, articles: &HashMap<u64, Article>) -> NewsThumbnails {
    let mut message = NewsThumbnails::new();
    message.thumbnails = feed
        .stories
        .iter()
        .filter_map(|story| {
            let rgb = articles.get(&story.id)?.thumbnail.as_ref()?;
            let mut t = NewsThumbnail::new();
            t.id = story.id;
            t.rgb = rgb.clone();
            t.width = client::THUMB_W;
            t.height = client::THUMB_H;
            Some(t)
        })
        .collect();
    message
}

/// The feed and its thumbnails kept for the next start.
fn keep(store: &Store, feed: &NewsFeed, articles: &HashMap<u64, Article>) {
    if let Err(err) = store.save(LAST_THUMBNAILS, &pictures(feed, articles)) {
        warn!("the thumbnails not kept: {err}");
    }
    if let Err(err) = store.save(LAST_FEED, feed) {
        warn!("the feed not kept: {err}");
    }
}

/// What the last feed and its thumbnails say of the stories' articles, so that they are not
/// read again. Without the thumbnails nothing is taken: a story would count as one without a
/// picture and its picture would never be read.
fn restore(store: &Store, feed: &NewsFeed) -> HashMap<u64, Article> {
    let Some(thumbnails) = store.load::<NewsThumbnails>(LAST_THUMBNAILS, LAST_MAX_AGE) else {
        return HashMap::new();
    };
    feed.stories
        .iter()
        .map(|s| {
            let thumbnail = thumbnails
                .thumbnails
                .iter()
                .find(|t| t.id == s.id && t.width == client::THUMB_W && t.height == client::THUMB_H && t.rgb.len() == (t.width * t.height * 3) as usize)
                .map(|t| t.rgb.clone());
            (s.id, Article { summary: s.summary.clone(), thumbnail })
        })
        .collect()
}

/// A story's article: its description, and its picture made into a thumbnail.
async fn read_article(story: &client::Story) -> Article {
    let meta = match client::page_meta(&story.url).await {
        Ok(meta) => meta,
        Err(err) => {
            debug!("article of {}: {err}", story.id);
            return Article::default();
        }
    };
    let thumbnail = match &meta.image {
        Some(url) => client::picture(url).await.map_err(|err| debug!("picture of {}: {err}", story.id)).ok(),
        None => None,
    };
    Article { summary: meta.description.unwrap_or_default(), thumbnail }
}

/// The stories as the catalog's message, with the summaries of the articles read so far.
fn to_message(stories: &[client::Story], articles: &HashMap<u64, Article>) -> NewsFeed {
    let mut feed = NewsFeed::new();
    feed.stories = stories
        .iter()
        .map(|s| {
            let mut m = NewsStory::new();
            m.id = s.id;
            m.title = cut(&s.title, 256);
            m.site = cut(&s.site, 64);
            m.url = if s.url.len() <= 512 { s.url.clone() } else { String::new() };
            m.summary = articles.get(&s.id).map(|a| a.summary.clone()).unwrap_or_default();
            m.points = s.points;
            m.comments = s.comments;
            m.by = cut(&s.by, 32);
            m.time = s.time;
            m
        })
        .collect();
    feed
}

/// `s` within the catalog's size, cut at a character.
fn cut(s: &str, max: usize) -> String {
    client::summary(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_carries_the_stories() {
        let story = client::Story { id: 7, title: "A".into(), url: "https://x.org/a".into(), site: "x.org".into(), by: "b".into(), points: 3, comments: 1, time: 9 };
        let articles = HashMap::from([(7, Article { summary: "Sum".into(), thumbnail: None })]);
        let feed = to_message(&[story], &articles);
        assert_eq!((feed.stories[0].id, feed.stories[0].summary.as_str(), feed.stories[0].site.as_str()), (7, "Sum", "x.org"));
    }
}
