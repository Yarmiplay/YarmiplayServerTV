//! Browse and delete items in a local Jellyfin library.
//!
//! Listing and artwork go through Jellyfin's API. Deleting an item uses
//! `DELETE /Items/{id}`, which removes the library entry and the files
//! together. Nothing here deletes on its own: the caller has to confirm first.

use super::api::JellyfinApi;
use serde::Serialize;
use serde_json::Value;

/// Artwork Jellyfin exposes as a still image. Backdrop is intentionally absent:
/// the browser asks for a poster, a logo, or an episode thumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Primary,
    Logo,
    Thumb,
}

impl ImageKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "Primary" => Some(Self::Primary),
            "Logo" => Some(Self::Logo),
            "Thumb" => Some(Self::Thumb),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "Primary",
            Self::Logo => "Logo",
            Self::Thumb => "Thumb",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    pub kind: String,
    pub tag: String,
}

/// One video file of an episode or movie. `id` is what a confirmed delete sends
/// to Jellyfin. `removes_item` is true when that delete also removes the episode
/// or movie, because the file has no separate Jellyfin id.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub removes_item: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaCard {
    pub id: String,
    pub name: String,
    /// Jellyfin's `Type`: Series, Season, Episode, Movie, Video, Folder, …
    pub kind: String,
    pub year: Option<i32>,
    pub index_number: Option<i32>,
    pub parent_index: Option<i32>,
    pub series_name: Option<String>,
    pub child_count: Option<i32>,
    /// Poster: Thumb for episodes and loose videos, otherwise Primary.
    pub poster: Option<ImageRef>,
    pub logo: Option<ImageRef>,
    pub files: Vec<MediaFile>,
    /// Opening it lists children (a show, a season, a folder) rather than files.
    pub can_drill: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaPage {
    pub items: Vec<MediaCard>,
    pub total: u32,
    pub start: u32,
}

/// Jellyfin ids are UUIDs. Reject anything else so a path can't be spliced into the URL.
pub fn require_item_id(id: &str) -> Result<&str, String> {
    let ok = (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if ok {
        Ok(id)
    } else {
        Err("That isn't a Jellyfin item".into())
    }
}

/// The control panel asks for a page at a time so artwork stays lazy.
pub fn page_limit(limit: u32) -> u32 {
    limit.clamp(1, 100)
}

/// Episodes and loose videos use the episode still (Thumb) when Jellyfin has one.
/// Shows, seasons and movies use the poster (Primary).
pub fn choose_poster<'a>(item_kind: &str, images: &'a [ImageRef]) -> Option<&'a ImageRef> {
    let order: &[&str] = if matches!(item_kind, "Episode" | "Video") {
        &["Thumb", "Primary"]
    } else {
        &["Primary", "Thumb"]
    };
    order
        .iter()
        .find_map(|kind| images.iter().find(|image| image.kind == *kind))
}

pub fn choose_logo(images: &[ImageRef]) -> Option<&ImageRef> {
    images.iter().find(|image| image.kind == "Logo")
}

pub fn can_drill(kind: &str) -> bool {
    matches!(
        kind,
        "Series"
            | "Season"
            | "Folder"
            | "BoxSet"
            | "CollectionFolder"
            | "Playlist"
            | "MusicAlbum"
            | "PhotoAlbum"
    )
}

/// Which id `DELETE /Items/{id}` should receive for one file, and whether that
/// removes the whole episode or movie.
pub fn file_delete_target(item_id: &str, source_id: &str, source_count: usize) -> (String, bool) {
    if source_count <= 1 || source_id.is_empty() || source_id == item_id {
        (item_id.to_string(), true)
    } else {
        (source_id.to_string(), false)
    }
}

/// Refuses the delete unless the control panel has already asked the user.
pub fn deletion_allowed(confirmed: bool) -> Result<(), String> {
    if confirmed {
        Ok(())
    } else {
        Err("Confirm the deletion before anything is removed".into())
    }
}

fn field<'a>(v: &'a Value, pascal: &str, camel: &str) -> Option<&'a Value> {
    v.get(pascal)
        .or_else(|| v.get(camel))
        .filter(|v| !v.is_null())
}

fn text(v: &Value, pascal: &str, camel: &str) -> String {
    field(v, pascal, camel)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn number(v: &Value, pascal: &str, camel: &str) -> Option<i32> {
    field(v, pascal, camel)
        .and_then(|v| v.as_i64())
        .and_then(|n| i32::try_from(n).ok())
}

fn image_refs(item: &Value) -> Vec<ImageRef> {
    let Some(tags) = field(item, "ImageTags", "imageTags").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    ["Primary", "Logo", "Thumb"]
        .into_iter()
        .filter_map(|kind| {
            let camel = match kind {
                "Primary" => "primary",
                "Logo" => "logo",
                "Thumb" => "thumb",
                _ => kind,
            };
            let tag = tags
                .get(kind)
                .or_else(|| tags.get(camel))
                .and_then(|v| v.as_str())
                .filter(|tag| !tag.is_empty())?;
            Some(ImageRef {
                kind: kind.to_string(),
                tag: tag.to_string(),
            })
        })
        .collect()
}

fn file_name(path: &str, fallback: &str) -> String {
    if !fallback.is_empty() {
        return fallback.to_string();
    }
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn media_files(item_id: &str, item: &Value) -> Vec<MediaFile> {
    let Some(sources) = field(item, "MediaSources", "mediaSources").and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let parsed: Vec<(String, String, String)> = sources
        .iter()
        .filter_map(|source| {
            let path = text(source, "Path", "path");
            if path.is_empty() {
                return None;
            }
            let source_id = text(source, "Id", "id");
            let name = file_name(&path, text(source, "Name", "name").as_str());
            Some((source_id, name, path))
        })
        .collect();
    let count = parsed.len();
    parsed
        .into_iter()
        .map(|(source_id, name, path)| {
            let (id, removes_item) = file_delete_target(item_id, &source_id, count);
            MediaFile {
                id,
                name,
                path,
                removes_item,
            }
        })
        .collect()
}

fn card(item: &Value) -> Option<MediaCard> {
    let id = text(item, "Id", "id");
    let name = text(item, "Name", "name");
    if require_item_id(&id).is_err() || name.is_empty() {
        return None;
    }
    let kind = text(item, "Type", "type");
    let images = image_refs(item);
    let files = if matches!(kind.as_str(), "Episode" | "Movie" | "Video") {
        media_files(&id, item)
    } else {
        Vec::new()
    };
    let series = text(item, "SeriesName", "seriesName");
    Some(MediaCard {
        poster: choose_poster(&kind, &images).cloned(),
        logo: choose_logo(&images).cloned(),
        id,
        name,
        kind: kind.clone(),
        year: number(item, "ProductionYear", "productionYear"),
        index_number: number(item, "IndexNumber", "indexNumber"),
        parent_index: number(item, "ParentIndexNumber", "parentIndexNumber"),
        series_name: if series.is_empty() {
            None
        } else {
            Some(series)
        },
        child_count: number(item, "ChildCount", "childCount"),
        files,
        can_drill: can_drill(&kind),
    })
}

pub fn parse_page(body: &Value, start: u32) -> MediaPage {
    let items: Vec<MediaCard> = field(body, "Items", "items")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(card).collect())
        .unwrap_or_default();
    let total = field(body, "TotalRecordCount", "totalRecordCount")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(items.len() as u32);
    MediaPage {
        items,
        total,
        start,
    }
}

impl JellyfinApi {
    /// One page of the children of a library, show, season or folder.
    pub async fn list_children(
        &self,
        user_id: Option<&str>,
        parent_id: &str,
        start: u32,
        limit: u32,
    ) -> Result<MediaPage, String> {
        let parent = require_item_id(parent_id)?;
        let limit = page_limit(limit);
        let user = match user_id {
            Some(id) if !id.is_empty() => Some(require_item_id(id)?),
            _ => None,
        };
        let path = match user {
            Some(id) => format!("/Users/{id}/Items"),
            None => "/Items".to_string(),
        };
        let query = [
            ("ParentId", parent.to_string()),
            ("SortBy", "SortName".into()),
            ("SortOrder", "Ascending".into()),
            ("Recursive", "false".into()),
            ("Fields", "MediaSources,Path".into()),
            ("EnableImageTypes", "Primary,Logo,Thumb".into()),
            ("StartIndex", start.to_string()),
            ("Limit", limit.to_string()),
        ];
        let response = Self::send(
            self.req(reqwest::Method::GET, &path).query(&query),
            "library",
        )
        .await?;
        let body: Value = response.json().await.map_err(|e| e.to_string())?;
        Ok(parse_page(&body, start))
    }

    /// `DELETE /Items/{id}`: Jellyfin removes the item and its files.
    pub async fn delete_item(&self, item_id: &str) -> Result<(), String> {
        let id = require_item_id(item_id)?;
        Self::send(
            self.req(reqwest::Method::DELETE, &format!("/Items/{id}")),
            "delete",
        )
        .await?;
        Ok(())
    }

    /// The image bytes for a poster, logo or thumb, sized for the control panel.
    pub async fn artwork(
        &self,
        item_id: &str,
        kind: ImageKind,
    ) -> Result<(Vec<u8>, String), String> {
        let id = require_item_id(item_id)?;
        let path = format!(
            "/Items/{id}/Images/{}?maxWidth=480&quality=80",
            kind.as_str()
        );
        let response = Self::send(self.req(reqwest::Method::GET, &path), "artwork").await?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .split(';')
            .next()
            .unwrap_or("image/jpeg")
            .to_string();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("artwork: {e}"))?;
        if bytes.is_empty() {
            return Err("artwork: empty image".into());
        }
        Ok((bytes.to_vec(), content_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn sample_page() -> Value {
        json!({
            "Items": [
                {
                    "Id": "11111111-1111-1111-1111-111111111111",
                    "Name": "Frieren",
                    "Type": "Series",
                    "ProductionYear": 2023,
                    "ChildCount": 2,
                    "ImageTags": { "Primary": "poster1", "Logo": "logo1", "Thumb": "wide1" }
                },
                {
                    "Id": "22222222-2222-2222-2222-222222222222",
                    "Name": "Season 1",
                    "Type": "Season",
                    "IndexNumber": 1,
                    "SeriesName": "Frieren",
                    "ImageTags": { "Primary": "season1" }
                },
                {
                    "Id": "33333333-3333-3333-3333-333333333333",
                    "Name": "The Journey's End",
                    "Type": "Episode",
                    "IndexNumber": 3,
                    "ParentIndexNumber": 1,
                    "SeriesName": "Frieren",
                    "ImageTags": { "Primary": "ep-primary", "Thumb": "ep-thumb" },
                    "MediaSources": [
                        {
                            "Id": "33333333-3333-3333-3333-333333333333",
                            "Name": "1080p",
                            "Path": "D:\\Shows\\Frieren\\S01E03.mkv"
                        },
                        {
                            "Id": "44444444-4444-4444-4444-444444444444",
                            "Name": "720p",
                            "Path": "D:\\Shows\\Frieren\\S01E03.720p.mkv"
                        }
                    ]
                }
            ],
            "TotalRecordCount": 3
        })
    }

    #[test]
    fn a_page_picks_posters_logos_and_files() {
        let page = parse_page(&sample_page(), 0);
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3);

        let show = &page.items[0];
        assert!(show.can_drill);
        assert_eq!(show.poster.as_ref().unwrap().kind, "Primary");
        assert_eq!(show.poster.as_ref().unwrap().tag, "poster1");
        assert_eq!(show.logo.as_ref().unwrap().tag, "logo1");
        assert!(show.files.is_empty());
        assert_eq!(show.year, Some(2023));

        let episode = &page.items[2];
        assert!(!episode.can_drill);
        assert_eq!(episode.poster.as_ref().unwrap().kind, "Thumb");
        assert_eq!(episode.poster.as_ref().unwrap().tag, "ep-thumb");
        assert_eq!(episode.files.len(), 2);
        assert!(episode.files[0].removes_item);
        assert_eq!(episode.files[0].id, episode.id);
        assert_eq!(episode.files[0].name, "1080p");
        assert!(!episode.files[1].removes_item);
        assert_eq!(episode.files[1].id, "44444444-4444-4444-4444-444444444444");
        assert_eq!(episode.files[1].name, "720p");
    }

    #[test]
    fn missing_art_has_no_poster_and_a_file_uses_its_path_name() {
        let page = parse_page(
            &json!({
                "Items": [{
                    "Id": "55555555-5555-5555-5555-555555555555",
                    "Name": "Dune",
                    "Type": "Movie",
                    "MediaSources": [{ "Path": "/films/Dune.mkv" }]
                }]
            }),
            0,
        );
        let movie = &page.items[0];
        assert!(movie.poster.is_none() && movie.logo.is_none());
        assert_eq!(movie.files[0].name, "Dune.mkv");
        assert!(movie.files[0].removes_item);
        assert_eq!(movie.files[0].id, movie.id);
    }

    #[test]
    fn only_a_confirmed_delete_is_allowed_and_ids_stay_uuids() {
        assert!(deletion_allowed(false).is_err());
        assert!(deletion_allowed(true).is_ok());
        assert!(require_item_id("../Items").is_err());
        assert!(require_item_id("").is_err());
        assert!(require_item_id("abcdef12").is_ok());
        assert!(ImageKind::parse("Backdrop").is_none());
        assert!(ImageKind::parse("Primary").is_some());
        assert_eq!(page_limit(0), 1);
        assert_eq!(page_limit(500), 100);
    }

    #[test]
    fn one_extra_version_deletes_that_version_only() {
        let (id, removes) = file_delete_target("item", "item", 2);
        assert_eq!(id, "item");
        assert!(removes);
        let (id, removes) = file_delete_target("item", "other", 2);
        assert_eq!(id, "other");
        assert!(!removes);
    }

    #[tokio::test]
    async fn listing_reads_children_and_delete_uses_the_item_api() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let seen_for_handler = Arc::clone(&seen);
        let base = serve(Arc::new(move |req: Request| {
            seen_for_handler.lock().unwrap().push(req);
            (
                200,
                r#"{"Items":[{"Id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","Name":"Film","Type":"Movie"}],"TotalRecordCount":1}"#
                    .into(),
            )
        }))
        .await;
        let api = JellyfinApi::with_base(&base, Some("tok".into()));
        let page = api
            .list_children(
                Some("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
                "cccccccc-cccc-cccc-cccc-cccccccccccc",
                48,
                500,
            )
            .await
            .unwrap();
        assert_eq!(page.items[0].name, "Film");
        assert_eq!(page.start, 48);

        api.delete_item("dddddddd-dddd-dddd-dddd-dddddddddddd")
            .await
            .unwrap();
        let artwork = api
            .artwork("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", ImageKind::Primary)
            .await
            .unwrap();
        assert_eq!(artwork.1, "text/plain");
        assert!(!artwork.0.is_empty());

        {
            let seen = seen.lock().unwrap();
            assert!(seen
                .iter()
                .all(|r| r.method != "DELETE" || r.path.starts_with("/Items/")));
            let list = &seen[0];
            assert_eq!(list.method, "GET");
            assert_eq!(
                list.path,
                "/Users/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb/Items"
            );
            assert_eq!(
                list.query.get("ParentId").map(String::as_str),
                Some("cccccccc-cccc-cccc-cccc-cccccccccccc")
            );
            assert_eq!(list.query.get("Limit").map(String::as_str), Some("100"));
            assert_eq!(list.query.get("StartIndex").map(String::as_str), Some("48"));
            assert!(list
                .headers
                .get("authorization")
                .is_some_and(|h| h.contains("Token=\"tok\"")));
            assert_eq!(seen[1].method, "DELETE",);
            assert_eq!(seen[1].path, "/Items/dddddddd-dddd-dddd-dddd-dddddddddddd");
            assert_eq!(seen[1].method, "DELETE");
            assert!(seen[2].path.contains("/Images/Primary"));
            assert_eq!(
                seen[2].query.get("maxWidth").map(String::as_str),
                Some("480")
            );
        }
        assert!(api.delete_item("not an id").await.is_err());
        assert_eq!(
            seen.lock().unwrap().len(),
            3,
            "a rejected id never reaches Jellyfin"
        );
    }
}
