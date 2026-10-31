use super::utils;
use crate::audio::is_audio_path;
use crate::slskd::models::SearchResponse;
use itertools::Itertools;
use shared::slskd::{AlbumResult, MatchResult, SearchResult, TrackResult};
use std::collections::HashMap;
use std::path::Path;

pub fn process_search_responses(
    responses: &[SearchResponse],
    searched_artist: &str,
    searched_album: Option<&str>,
    expected_tracks: &[&str],
    // Strict mode: drop every file that is not FLAC before matching.
    require_flac: bool,
    // Drop every file whose path does not name the searched artist: set for
    // the artist-less retry, whose query no longer filtered by artist.
    require_artist: bool,
) -> Vec<AlbumResult> {
    const MIN_SCORE_THRESHOLD: f64 = 0.6;
    let scored_files: Vec<(MatchResult, SearchResult)> = responses
        .iter()
        .flat_map(|resp| {
            resp.files.iter().filter_map(|file| {
                let path = Path::new(&file.filename);
                if require_flac && !is_flac_path(path) {
                    return None;
                }
                if path.extension().is_some() && !is_audio_path(path) {
                    return None;
                }

                let rank_result = utils::rank_match(
                    &file.filename,
                    Some(searched_artist),
                    searched_album,
                    expected_tracks,
                );

                if rank_result.total_score < MIN_SCORE_THRESHOLD {
                    return None;
                }
                // No word of the searched artist in any folder or the file
                // name: the path does not confirm the artist.
                if require_artist && rank_result.artist_score <= 0.0 {
                    return None;
                }

                let search_result = SearchResult {
                    username: resp.username.clone(),
                    filename: file.filename.clone(),
                    size: file.size,
                    bitrate: file.bit_rate,
                    duration: file.length,
                    sample_rate: file.sample_rate,
                    bit_depth: file.bit_depth,
                    has_free_upload_slot: resp.has_free_upload_slot,
                    upload_speed: resp.upload_speed,
                    queue_length: resp.queue_length,
                };
                Some((rank_result, search_result))
            })
        })
        .collect();

    find_best_albums(&scored_files, expected_tracks)
}

/// A file whose extension is `flac`, in any case. One without an extension
/// is not.
fn is_flac_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("flac"))
}

fn find_best_albums(
    scored_files: &[(MatchResult, SearchResult)],
    expected_tracks: &[&str],
) -> Vec<AlbumResult> {
    if expected_tracks.is_empty() {
        return vec![];
    }

    let album_groups = scored_files.iter().into_group_map_by(|(rank, search)| {
        (
            search.username.clone(),
            rank.guessed_artist.clone(),
            rank.guessed_album.clone(),
        )
    });

    album_groups
        .into_iter()
        .filter_map(|((username, artist, album_title), files_in_group)| {
            // Specific search: find the single best file for each expected track.
            let mut best_files_for_album = HashMap::new();

            for expected_track_title in expected_tracks {
                if let Some(best_file_for_track) = files_in_group
                    .iter()
                    // Find all files that matched this specific track
                    .filter(|(rank, _)| &rank.matched_track == expected_track_title)
                    // Find the best one among them
                    .max_by(|(r1, s1), (r2, s2)| {
                        r1.total_score
                            .partial_cmp(&r2.total_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| {
                                s1.quality_score().partial_cmp(&s2.quality_score()).unwrap()
                            })
                    })
                {
                    best_files_for_album.insert(*expected_track_title, best_file_for_track);
                }
            }

            let final_tracks: Vec<_> = expected_tracks
                .iter()
                .filter_map(|t| best_files_for_album.get(*t))
                .map(|(mr, sr)| TrackResult::new(sr.clone(), mr.clone()))
                .collect();

            if final_tracks.is_empty() {
                return None;
            }

            let completeness = if !expected_tracks.is_empty() {
                final_tracks.len() as f64 / expected_tracks.len() as f64
            } else {
                1.0
            };

            let total_size: i64 = final_tracks.iter().map(|t| t.base.size).sum();
            let dominant_quality = final_tracks
                .iter()
                .map(|t| t.base.quality())
                .counts()
                .into_iter()
                .max_by_key(|&(_, count)| count)
                .map(|(val, _)| val)
                .unwrap_or_default();

            let first_track = final_tracks[0].base.clone();
            let album_path = first_track.filename.clone();

            let avg_score: f64 =
                final_tracks.iter().map(|t| t.match_score).sum::<f64>() / final_tracks.len() as f64;
            let avg_format_score = final_tracks
                .iter()
                .map(|t| t.base.quality_score())
                .sum::<f64>()
                / final_tracks.len() as f64;

            let album_quality_score =
                (avg_score * 0.3) + (completeness * 0.3) + (avg_format_score * 0.4);

            Some(AlbumResult {
                username,
                album_path,
                album_title,
                artist: Some(artist),
                track_count: final_tracks.len(),
                total_size,
                tracks: final_tracks,
                dominant_quality,
                has_free_upload_slot: first_track.has_free_upload_slot,
                upload_speed: first_track.upload_speed,
                queue_length: first_track.queue_length,
                score: album_quality_score,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slskd::models::SearchResponseFile;

    const ARTIST: &str = "Kowloon";
    const ALBUM: &str = "Come Over";
    const TRACKS: [&str; 2] = ["Northern Line", "Glass Atlas"];

    fn file(name: &str) -> SearchResponseFile {
        SearchResponseFile {
            filename: name.to_string(),
            size: 27_213_250,
            bit_rate: None,
            length: Some(200),
            sample_rate: Some(44100),
            bit_depth: Some(16),
        }
    }

    fn peer(username: &str, files: &[&str]) -> SearchResponse {
        SearchResponse {
            username: username.to_string(),
            files: files.iter().map(|f| file(f)).collect(),
            has_free_upload_slot: true,
            upload_speed: 1000,
            queue_length: 0,
        }
    }

    fn album_files(folder: &str, ext: &str) -> Vec<String> {
        TRACKS
            .iter()
            .enumerate()
            .map(|(i, t)| format!("Music\\{ARTIST} - {ALBUM}\\{folder}\\0{} - {t}{ext}", i + 1))
            .collect()
    }

    fn search(responses: &[SearchResponse], require_flac: bool) -> Vec<AlbumResult> {
        process_search_responses(responses, ARTIST, Some(ALBUM), &TRACKS, require_flac, false)
    }

    fn sources(albums: &[AlbumResult]) -> Vec<String> {
        let mut users: Vec<String> = albums.iter().map(|a| a.username.clone()).collect();
        users.sort();
        users.dedup();
        users
    }

    fn filenames(albums: &[AlbumResult]) -> Vec<String> {
        let mut names: Vec<String> = albums
            .iter()
            .flat_map(|a| a.tracks.iter().map(|t| t.base.filename.clone()))
            .collect();
        names.sort();
        names
    }

    fn refs(names: &[String]) -> Vec<&str> {
        names.iter().map(String::as_str).collect()
    }

    #[test]
    fn without_the_flag_flac_and_mp3_peers_both_stay() {
        let flac = album_files("FLAC", ".flac");
        let mp3 = album_files("MP3", ".mp3");
        let responses = [peer("lossless", &refs(&flac)), peer("lossy", &refs(&mp3))];

        assert_eq!(
            sources(&search(&responses, false)),
            vec!["lossless", "lossy"]
        );
    }

    #[test]
    fn requiring_flac_drops_an_mp3_only_peer() {
        let flac = album_files("FLAC", ".flac");
        let mp3 = album_files("MP3", ".mp3");
        let responses = [peer("lossless", &refs(&flac)), peer("lossy", &refs(&mp3))];

        assert_eq!(sources(&search(&responses, true)), vec!["lossless"]);
    }

    #[test]
    fn requiring_flac_keeps_only_the_flac_files_of_a_mixed_folder() {
        // One folder holding the first track as FLAC (upper-case extension)
        // and the second only as MP3.
        let mixed = vec![
            album_files("Mixed", ".FLAC").remove(0),
            album_files("Mixed", ".mp3").remove(1),
        ];
        let responses = [peer("mixed", &refs(&mixed))];

        assert_eq!(filenames(&search(&responses, false)).len(), 2);
        assert_eq!(filenames(&search(&responses, true)), vec![mixed[0].clone()]);
    }

    #[test]
    fn requiring_flac_drops_a_file_without_an_extension() {
        let bare = album_files("Bare", "");
        let responses = [peer("bare", &refs(&bare))];

        assert_eq!(sources(&search(&responses, false)), vec!["bare"]);
        assert!(search(&responses, true).is_empty());
    }

    /// The same album title and track names, shared from another artist's
    /// folder.
    fn other_artist_album() -> Vec<String> {
        TRACKS
            .iter()
            .enumerate()
            .map(|(i, t)| format!("Music\\Other Band - {ALBUM}\\0{} - {t}.flac", i + 1))
            .collect()
    }

    fn album_search(responses: &[SearchResponse], require_artist: bool) -> Vec<AlbumResult> {
        process_search_responses(
            responses,
            ARTIST,
            Some(ALBUM),
            &TRACKS,
            false,
            require_artist,
        )
    }

    #[test]
    fn requiring_the_artist_drops_another_artists_same_titled_album() {
        let other = other_artist_album();
        let responses = [peer("elsewhere", &refs(&other))];

        assert!(album_search(&responses, true).is_empty());
    }

    #[test]
    fn requiring_the_artist_keeps_the_artists_own_album() {
        let own = album_files("FLAC", ".flac");
        let other = other_artist_album();
        let responses = [peer("own", &refs(&own)), peer("elsewhere", &refs(&other))];

        assert_eq!(sources(&album_search(&responses, true)), vec!["own"]);
    }

    #[test]
    fn without_requiring_the_artist_matching_is_unchanged() {
        // Today an exact title by the wrong artist still clears the score
        // threshold; a normal search keeps it.
        let other = other_artist_album();
        let responses = [peer("elsewhere", &refs(&other))];
        assert_eq!(sources(&album_search(&responses, false)), vec!["elsewhere"]);

        let responses = [peer(
            "elsewhere",
            &[r"Music\Other Band\Somewhere\05 - Paper Lanterns.flac"],
        )];
        let albums = process_search_responses(
            &responses,
            "Static Harbor",
            None,
            &["Paper Lanterns"],
            false,
            false,
        );
        assert_eq!(
            filenames(&albums),
            vec![r"Music\Other Band\Somewhere\05 - Paper Lanterns.flac"]
        );
    }

    #[test]
    fn requiring_the_artist_drops_another_artists_same_titled_track() {
        let responses = [peer(
            "elsewhere",
            &[
                r"Music\Other Band\Somewhere\05 - Paper Lanterns.flac",
                r"Music\Static Harbor\Glass Atlas\02 - Paper Lanterns.flac",
            ],
        )];
        let albums = process_search_responses(
            &responses,
            "Static Harbor",
            None,
            &["Paper Lanterns"],
            false,
            true,
        );
        assert_eq!(
            filenames(&albums),
            vec![r"Music\Static Harbor\Glass Atlas\02 - Paper Lanterns.flac"]
        );
    }
}
