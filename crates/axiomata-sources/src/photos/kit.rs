//! The second way into the owner's photos: Apple's PhotoKit, taken only when the library's own stores are of a form
//! not known or not where they were (a newer Photos, [`PhotoError::UnknownFormat`]) — never while Photos is still
//! sorting a library it knows ([`PhotoError::NotIndexed`]). It reads days, kinds, favourites and albums — never the
//! words of Photos' search, which only the stores know: a read by words fails as [`PhotoError::NoWords`], so nothing
//! is guessed. PhotoKit leaves out hidden pictures, the trash and the takes of a burst Photos does not show; a closed
//! album's pictures are left out here as there, and a screenshot unless asked for — by its mark or by Photos' album of
//! them. It knows no place names, so none is told, and it cannot tell a document or a passport from a photo: those
//! the stores' read leaves out may come along here. macOS asks the owner once whether the app may see the photos —
//! only an app that says why in its `Info.plist`; any other gets [`PhotoError::NoPhotosAccess`] without asking, as
//! asking would end it.

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSBundle, NSDate, NSPredicate, NSSortDescriptor, NSString};
use objc2_photos::{
    PHAccessLevel, PHAsset, PHAssetCollection, PHAssetCollectionSubtype, PHAssetCollectionType,
    PHAssetMediaSubtype, PHAssetMediaType, PHAssetSourceType, PHAuthorizationStatus,
    PHFetchOptions, PHFetchResult, PHPhotoLibrary,
};

use super::{Found, Kind, MAX_READ, Photo, PhotoError, Query, clean, same_name, short};

/// How long the owner gets to answer the system's dialog.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
/// What an app must say in its `Info.plist` before macOS lets it ask for the photos.
const USAGE_KEY: &str = "NSPhotoLibraryUsageDescription";

/// Whether this app may see the photos: asked once when macOS never asked; refused stays refused.
fn ask() -> Result<(), PhotoError> {
    // SAFETY: a class method taking a plain constant; it reads the app's own state only.
    match unsafe { PHPhotoLibrary::authorizationStatusForAccessLevel(PHAccessLevel::ReadWrite) } {
        PHAuthorizationStatus::Authorized | PHAuthorizationStatus::Limited => return Ok(()),
        PHAuthorizationStatus::NotDetermined => {}
        _ => return Err(PhotoError::NoPhotosAccess),
    }
    // A program without the reason in its bundle (a binary run from a terminal) is ended by macOS when it asks.
    let reason = NSBundle::mainBundle().objectForInfoDictionaryKey(&NSString::from_str(USAGE_KEY));
    if reason.is_none() {
        return Err(PhotoError::NoPhotosAccess);
    }
    let (tx, rx) = mpsc::channel();
    let answer = RcBlock::new(move |status: PHAuthorizationStatus| {
        // The receiver may be gone after a timeout; nobody is left to tell.
        let _ = tx.send(status);
    });
    // SAFETY: the block lives across the call and PhotoKit copies it; the level is a plain constant.
    unsafe {
        PHPhotoLibrary::requestAuthorizationForAccessLevel_handler(
            PHAccessLevel::ReadWrite,
            &answer,
        );
    }
    match rx.recv_timeout(ANSWER_TIMEOUT) {
        Ok(PHAuthorizationStatus::Authorized | PHAuthorizationStatus::Limited) => Ok(()),
        _ => Err(PhotoError::NoPhotosAccess),
    }
}

/// The owner's own albums, by their names.
pub(super) fn albums() -> Result<Vec<String>, PhotoError> {
    ask()?;
    let mut names: Vec<String> = own_albums()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    Ok(names)
}

/// What `query` asks for, said as the stores' read takes it: the album trimmed (an empty one is none), only words
/// with something in them, a limit of 1 to [`MAX_READ`].
fn asked(query: &Query) -> (Option<&str>, bool, usize) {
    let album = query
        .album
        .as_deref()
        .map(str::trim)
        .filter(|album| !album.is_empty());
    let words = query.words.iter().any(|word| !word.trim().is_empty());
    (album, words, query.limit.clamp(1, MAX_READ))
}

/// Reads what `query` asks for, the newest first; never a picture of an album named in `closed`.
pub(super) fn read(query: &Query, closed: &[String]) -> Result<Found, PhotoError> {
    let (album, words, limit) = asked(query);
    if words {
        return Err(PhotoError::NoWords);
    }
    if let Some(album) = album
        && closed.iter().any(|name| same_name(name, album))
    {
        return Err(PhotoError::AlbumClosed);
    }
    ask()?;
    let albums = own_albums();
    let named = |name: &str| -> Vec<&Retained<PHAssetCollection>> {
        albums
            .iter()
            .filter(|(title, _)| same_name(title, name))
            .map(|(_, collection)| collection)
            .collect()
    };
    // A closed album must still be known: one renamed or gone would let its pictures through unseen.
    let mut shut: HashSet<String> = HashSet::new();
    for name in closed {
        let collections = named(name);
        if collections.is_empty() {
            return Err(PhotoError::AlbumClosed);
        }
        for collection in collections {
            // All its pictures, of any day.
            shut.extend(ids_in(&assets_in(collection, &options(None, false))));
        }
    }
    // Photos' album of screenshots knows some its mark does not: those stay out unless screenshots were asked for.
    let screenshots: Vec<Retained<PHFetchResult<PHAsset>>> = smart_screenshots()
        .iter()
        .map(|collection| assets_in(collection, &options(Some(query), query.favorites)))
        .collect();
    let mut taken_for_screenshots: HashSet<String> = HashSet::new();
    if !query.screenshots {
        for result in &screenshots {
            taken_for_screenshots.extend(ids_in(result));
        }
    }
    let wanted: Vec<Retained<PHFetchResult<PHAsset>>> = match album {
        Some(album) => {
            let collections = named(album);
            if collections.is_empty() {
                return Ok(Found {
                    unknown: vec![format!("album {}", short(&clean(album)))],
                    ..Found::default()
                });
            }
            collections
                .into_iter()
                .map(|collection| assets_in(collection, &options(Some(query), query.favorites)))
                .collect()
        }
        None if query.screenshots => screenshots,
        // SAFETY: options made here, of the kind a fetch of assets takes.
        None => vec![unsafe {
            PHAsset::fetchAssetsWithOptions(Some(&options(Some(query), query.favorites)))
        }],
    };
    let mut seen = HashSet::new();
    let mut photos = Vec::new();
    for result in &wanted {
        for index in 0..count(result) {
            // SAFETY: an index below the result's count.
            let asset = unsafe { result.objectAtIndex(index) };
            let id = id_of(&asset);
            if shut.contains(&id) || taken_for_screenshots.contains(&id) || !seen.insert(id.clone())
            {
                continue;
            }
            let Some(mut photo) = photo_of(&asset, id) else {
                continue;
            };
            if query.screenshots {
                // Asked for screenshots: what Photos' album of them holds is one, marked or not.
                if photo.kind == Kind::Photo && album.is_none() {
                    photo.kind = Kind::Screenshot;
                }
                if photo.kind != Kind::Screenshot {
                    continue;
                }
            } else if photo.kind == Kind::Screenshot {
                continue;
            }
            photos.push(photo);
        }
    }
    photos.sort_by_key(|photo| std::cmp::Reverse(photo.at));
    let total = photos.len();
    photos.truncate(limit);
    Ok(Found {
        photos,
        total,
        unknown: Vec::new(),
    })
}

/// The owner's own albums with their names, also those in folders.
fn own_albums() -> Vec<(String, Retained<PHAssetCollection>)> {
    // SAFETY: plain constants for the kind of collection; no options.
    let found = unsafe {
        PHAssetCollection::fetchAssetCollectionsWithType_subtype_options(
            PHAssetCollectionType::Album,
            PHAssetCollectionSubtype::AlbumRegular,
            None,
        )
    };
    (0..count(&found))
        .map(|index| {
            // SAFETY: an index below the result's count.
            let collection = unsafe { found.objectAtIndex(index) };
            // SAFETY: a property of a fetched collection, read on the thread that fetched it.
            let name = unsafe { collection.localizedTitle() }
                .map(|title| clean(&title.to_string()))
                .unwrap_or_default();
            (name, collection)
        })
        .collect()
}

/// Photos' own album of screenshots.
fn smart_screenshots() -> Vec<Retained<PHAssetCollection>> {
    // SAFETY: plain constants for the kind of collection; no options.
    let found = unsafe {
        PHAssetCollection::fetchAssetCollectionsWithType_subtype_options(
            PHAssetCollectionType::SmartAlbum,
            PHAssetCollectionSubtype::SmartAlbumScreenshots,
            None,
        )
    };
    (0..count(&found))
        // SAFETY: an index below the result's count.
        .map(|index| unsafe { found.objectAtIndex(index) })
        .collect()
}

fn assets_in(
    collection: &PHAssetCollection,
    options: &PHFetchOptions,
) -> Retained<PHFetchResult<PHAsset>> {
    // SAFETY: a fetched collection and options made here, of the kind a fetch of assets takes.
    unsafe { PHAsset::fetchAssetsInAssetCollection_options(collection, Some(options)) }
}

/// The ids of every picture of `result`.
fn ids_in(result: &PHFetchResult<PHAsset>) -> HashSet<String> {
    (0..count(result))
        // SAFETY: an index below the result's count.
        .map(|index| id_of(&*unsafe { result.objectAtIndex(index) }))
        .collect()
}

/// What a fetch takes: the days of `query` (any day without one), the newest first, only the owner's own library.
/// Hidden pictures and the takes of a burst stay out as PhotoKit leaves them out by default — said here again, so no
/// change of a default lets them in.
fn options(query: Option<&Query>, favorites: bool) -> Retained<PHFetchOptions> {
    let mut parts = Vec::new();
    let mut arguments: Vec<Retained<AnyObject>> = Vec::new();
    if let Some(query) = query {
        parts.push("creationDate >= %@ AND creationDate < %@");
        arguments.push(Retained::into_super(Retained::into_super(date(
            query.since,
        ))));
        arguments.push(Retained::into_super(Retained::into_super(date(
            query.until,
        ))));
    }
    if favorites {
        parts.push("favorite == YES");
    }
    // SAFETY: a plain constructor and setters on the object made here. The predicate's format is built only of the
    // constant parts above — never of anything asked — with one date argument for each `%@`, so it cannot fail to
    // parse (which would raise an exception).
    unsafe {
        let options = PHFetchOptions::new();
        options.setIncludeHiddenAssets(false);
        options.setIncludeAllBurstAssets(false);
        options.setIncludeAssetSourceTypes(PHAssetSourceType::TypeUserLibrary);
        if !parts.is_empty() {
            let arguments = NSArray::from_retained_slice(&arguments);
            let predicate = NSPredicate::predicateWithFormat_argumentArray(
                &NSString::from_str(&parts.join(" AND ")),
                Some(&arguments),
            );
            options.setPredicate(Some(&predicate));
        }
        let newest = NSSortDescriptor::sortDescriptorWithKey_ascending(
            Some(&NSString::from_str("creationDate")),
            false,
        );
        options.setSortDescriptors(Some(&NSArray::from_retained_slice(&[newest])));
        options
    }
}

/// Seconds since 1970 as a date of Foundation's.
fn date(seconds: i64) -> Retained<NSDate> {
    #[allow(clippy::cast_precision_loss)]
    NSDate::dateWithTimeIntervalSince1970(seconds as f64)
}

fn count<T: objc2::Message>(result: &PHFetchResult<T>) -> usize {
    // SAFETY: a property of a fetch result.
    unsafe { result.count() }
}

/// Photos' own id of a picture: the first part of PhotoKit's ("UUID/L0/001"), as the stores and `show` know it.
fn id_of(asset: &PHAsset) -> String {
    // SAFETY: a property of a fetched asset.
    let id = unsafe { asset.localIdentifier() }.to_string();
    id.split('/').next().unwrap_or_default().to_uppercase()
}

fn photo_of(asset: &PHAsset, uuid: String) -> Option<Photo> {
    if !super::plain_uuid(&uuid) {
        return None;
    }
    // SAFETY: properties of a fetched asset.
    let (kind, subtypes, created, favorite) = unsafe {
        (
            asset.mediaType(),
            asset.mediaSubtypes(),
            asset.creationDate(),
            asset.isFavorite(),
        )
    };
    let kind = match kind {
        PHAssetMediaType::Video => Kind::Video,
        PHAssetMediaType::Image if subtypes.contains(PHAssetMediaSubtype::PhotoScreenshot) => {
            Kind::Screenshot
        }
        PHAssetMediaType::Image => Kind::Photo,
        _ => return None,
    };
    #[allow(clippy::cast_possible_truncation)]
    let at = created?.timeIntervalSince1970() as i64;
    Some(Photo {
        uuid,
        at,
        kind,
        favorite,
        town: None,
        country: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_asked_is_read_as_the_stores_read_it() {
        let query = Query {
            album: Some("  ".into()),
            words: vec![" ".into()],
            limit: 0,
            ..Query::default()
        };
        assert_eq!(asked(&query), (None, false, 1));
        let query = Query {
            album: Some(" Urlaub ".into()),
            words: vec!["Strand".into()],
            limit: 1000,
            ..Query::default()
        };
        assert_eq!(asked(&query), (Some("Urlaub"), true, MAX_READ));
    }

    #[test]
    fn words_and_a_closed_album_are_refused_before_photos_is_asked() {
        let by_words = Query {
            words: vec!["Strand".into()],
            ..Query::default()
        };
        assert_eq!(read(&by_words, &[]), Err(PhotoError::NoWords));
        let closed_album = Query {
            album: Some("übersee".into()),
            ..Query::default()
        };
        assert_eq!(
            read(&closed_album, &["Übersee".into()]),
            Err(PhotoError::AlbumClosed)
        );
    }
}
