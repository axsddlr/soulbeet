// MusicBrainz /ws/2 double. Both consumers speak the JSON web service:
// - soulbeet (musicbrainz_rs): recording / release-group search with the
//   entities inlined, plus release lookup for tracklists
// - beets 2.x (musicbrainz plugin): recording search for candidate ids,
//   then per-id recording lookups
// Everything is served from the shared fixture dataset. A control API sets
// how beets is answered and lists what beets asked for (see MbControl);
// soulbeet is always answered normally.

import type { IncomingMessage, Server } from 'node:http';
import { setTimeout as sleep } from 'node:timers/promises';
import {
  findRecording,
  findRelease,
  monitorArtistMbid,
  monitorArtistName,
  monitorReleases,
  releases,
  type FixtureRelease,
  type FixtureTrack,
} from '../../fixtures/dataset.js';
import { createStubServer, sendJson, type RouteContext } from './router.js';

// ---- control ------------------------------------------------------------------

/**
 * normal: the dataset as is. weak: every recording is a poor match (a live
 * title and three times the length), so beets skips with candidates.
 * unavailable: 503 for everything, which beets reports as a lookup error.
 */
export type MbMode = 'normal' | 'weak' | 'unavailable';

const MB_MODES: MbMode[] = ['normal', 'weak', 'unavailable'];

export class MbControl {
  mode: MbMode = 'normal';
  /** delay before every answer to beets */
  latencyMs = 0;
  /** paths beets asked for since the last reset, oldest first */
  requests: string[] = [];
  /** release groups added at runtime through /_control/release-group */
  added: FixtureRelease[] = [];

  reset(): void {
    this.added = [];
    this.mode = 'normal';
    this.latencyMs = 0;
    this.requests = [];
  }
}

/** beets 2.11 sends "beets/2.11.0 https://beets.io/"; soulbeet sends "Soulbeet/...". */
function isBeets(req: IncomingMessage): boolean {
  return req.headers['user-agent']?.startsWith('beets/') ?? false;
}

const WEAK_LENGTH_FACTOR = 3;

/** A track's title as weak mode serves it to beets. */
export function weakTitle(title: string): string {
  return `${title} (Live at Somewhere)`;
}

function weakened(release: FixtureRelease): FixtureRelease {
  return {
    ...release,
    tracks: release.tracks.map((track) => ({
      ...track,
      title: weakTitle(track.title),
      lengthMs: track.lengthMs * WEAK_LENGTH_FACTOR,
    })),
  };
}

function weakHit(hit: RecordingHit): RecordingHit {
  const release = weakened(hit.release);
  const track = release.tracks.find((t) => t.mbid === hit.track.mbid) as FixtureTrack;
  return { release, track };
}

// ---- Lucene query handling -------------------------------------------------

/**
 * Parses the Lucene subset the two clients emit: `field:"phrase"` pairs
 * (musicbrainz_rs) and `field:(escaped terms)` groups (beets), joined with
 * AND or whitespace.
 */
function parseLucene(query: string): { fields: Map<string, string>; freeText: string[] } {
  const fields = new Map<string, string>();
  const fieldPattern = /([a-z]+):(?:"([^"]*)"|\(([^)]*)\)|(\S+))/gi;
  let match: RegExpExecArray | null;
  const consumed: string[] = [];
  while ((match = fieldPattern.exec(query)) !== null) {
    const key = (match[1] ?? '').toLowerCase();
    const value = match[2] ?? match[3] ?? match[4] ?? '';
    // beets escapes Lucene metacharacters with backslashes; undo that.
    fields.set(key, value.replace(/\\(.)/g, '$1').toLowerCase().trim());
    consumed.push(match[0] as string);
  }
  let rest = query;
  for (const piece of consumed) {
    rest = rest.replace(piece, ' ');
  }
  const freeText = rest
    .replace(/\bAND\b|\bOR\b|\bNOT\b/g, ' ')
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
  return { fields, freeText };
}

function matchesText(haystack: string, needle: string | undefined): boolean {
  if (needle === undefined || needle.length === 0) {
    return true;
  }
  return haystack.toLowerCase().includes(needle);
}

interface RecordingHit {
  release: FixtureRelease;
  track: FixtureTrack;
}

function searchRecordings(query: string): RecordingHit[] {
  const { fields, freeText } = parseLucene(query);
  const title = fields.get('recording') ?? fields.get('recordingaccent');
  const artist = fields.get('artistname') ?? fields.get('artist');
  const free = freeText.join(' ');
  const hasCriteria = title !== undefined || artist !== undefined || free.length > 0;

  const hits: RecordingHit[] = [];
  for (const release of releases) {
    for (const track of release.tracks) {
      const titleOk = matchesText(track.title, title);
      const artistOk = matchesText(release.artist, artist);
      const freeOk =
        title === undefined && artist === undefined
          ? matchesText(`${release.artist} ${track.title}`, free)
          : true;
      if (hasCriteria && titleOk && artistOk && freeOk) {
        hits.push({ release, track });
      }
    }
  }
  return hits;
}

function searchReleaseGroups(query: string): FixtureRelease[] {
  const { fields, freeText } = parseLucene(query);
  const title =
    fields.get('releasegroup') ?? fields.get('releasegroupaccent') ?? fields.get('release');
  const artist = fields.get('artist') ?? fields.get('artistname');
  const free = freeText.join(' ');
  const hasCriteria = title !== undefined || artist !== undefined || free.length > 0;

  return releases.filter((release) => {
    const titleOk = matchesText(release.title, title);
    const artistOk = matchesText(release.artist, artist);
    const freeOk =
      title === undefined && artist === undefined
        ? matchesText(`${release.artist} ${release.title}`, free)
        : true;
    return hasCriteria && titleOk && artistOk && freeOk;
  });
}

// ---- JSON entity builders ----------------------------------------------------

function artistCreditJson(release: FixtureRelease): Array<Record<string, unknown>> {
  return [
    {
      name: release.artist,
      joinphrase: '',
      artist: {
        id: release.artistMbid,
        name: release.artist,
        'sort-name': release.artist,
        disambiguation: '',
        type: 'Group',
      },
    },
  ];
}

function releaseJson(release: FixtureRelease): Record<string, unknown> {
  return {
    id: release.releaseMbid,
    title: release.title,
    status: 'Official',
    date: release.date,
    country: 'XW',
    disambiguation: '',
    quality: 'normal',
    barcode: null,
    packaging: null,
    'status-id': '4e304316-386d-3409-af2e-78857eec5cfe',
    'release-events': [{ date: release.date, area: null }],
    'text-representation': { language: 'eng', script: 'Latn' },
    'artist-credit': artistCreditJson(release),
    'release-group': {
      id: release.releaseGroupMbid,
      title: release.title,
      'primary-type': release.primaryType,
      'first-release-date': release.date,
      'secondary-types': release.secondaryTypes ?? [],
      disambiguation: '',
    },
  };
}

function recordingJson(hit: RecordingHit, withReleases: boolean): Record<string, unknown> {
  return {
    id: hit.track.mbid,
    title: hit.track.title,
    length: hit.track.lengthMs,
    video: false,
    disambiguation: '',
    'first-release-date': hit.release.date,
    'artist-credit': artistCreditJson(hit.release),
    aliases: [],
    isrcs: [],
    relations: [],
    ...(withReleases ? { releases: [releaseJson(hit.release)] } : {}),
    rating: { 'votes-count': 12, value: hit.release.rating / 20 },
    score: 100,
  };
}

function releaseGroupJson(release: FixtureRelease): Record<string, unknown> {
  return {
    id: release.releaseGroupMbid,
    title: release.title,
    'primary-type': release.primaryType,
    'secondary-types': release.secondaryTypes ?? [],
    'first-release-date': release.date,
    disambiguation: '',
    'artist-credit': artistCreditJson(release),
    releases: [releaseJson(release)],
    rating: { 'votes-count': 12, value: release.rating / 20 },
    score: 100,
  };
}

// ---- artist monitoring ----------------------------------------------------------

interface FixtureArtist {
  id: string;
  name: string;
}

/** Everything the stub knows about, including groups added at runtime. */
function catalog(control: MbControl): FixtureRelease[] {
  return [...releases, ...monitorReleases, ...control.added];
}

function artists(control: MbControl): FixtureArtist[] {
  const byId = new Map<string, FixtureArtist>();
  for (const release of catalog(control)) {
    byId.set(release.artistMbid, { id: release.artistMbid, name: release.artist });
  }
  byId.set(monitorArtistMbid, { id: monitorArtistMbid, name: monitorArtistName });
  return [...byId.values()];
}

function artistJson(artist: FixtureArtist): Record<string, unknown> {
  return {
    id: artist.id,
    name: artist.name,
    'sort-name': artist.name,
    disambiguation: '',
    country: 'XW',
    type: 'Group',
    'type-id': 'e431f5f6-b5d2-343d-8b36-72607fffb74b',
    score: 100,
  };
}

function searchArtists(control: MbControl, query: string): FixtureArtist[] {
  const { fields, freeText } = parseLucene(query);
  const name = fields.get('artist') ?? fields.get('artistname') ?? fields.get('name');
  const free = freeText.join(' ');
  if (name === undefined && free.length === 0) {
    return [];
  }
  return artists(control).filter(
    (artist) =>
      matchesText(artist.name, name) && (name !== undefined || matchesText(artist.name, free)),
  );
}

function browseReleaseGroups(control: MbControl, url: URL): Record<string, unknown> {
  const artistMbid = url.searchParams.get('artist') ?? '';
  const types = (url.searchParams.get('type') ?? '')
    .split('|')
    .map((t) => t.trim().toLowerCase())
    .filter((t) => t.length > 0);
  const limitParam = Number(url.searchParams.get('limit') ?? '25');
  const limit = Number.isFinite(limitParam) && limitParam > 0 ? limitParam : 25;
  const offsetParam = Number(url.searchParams.get('offset') ?? '0');
  const offset = Number.isFinite(offsetParam) && offsetParam > 0 ? offsetParam : 0;

  const groups = new Map<string, FixtureRelease>();
  for (const release of catalog(control)) {
    if (
      release.artistMbid === artistMbid &&
      (types.length === 0 || types.includes(release.primaryType.toLowerCase()))
    ) {
      groups.set(release.releaseGroupMbid, release);
    }
  }
  const sorted = [...groups.values()].sort((a, b) => a.date.localeCompare(b.date));
  console.log(`[mb] release-group browse artist ${artistMbid} -> ${sorted.length}`);
  return {
    'release-group-count': sorted.length,
    'release-group-offset': offset,
    'release-groups': sorted.slice(offset, offset + limit).map((r) => ({
      id: r.releaseGroupMbid,
      title: r.title,
      'primary-type': r.primaryType,
      'secondary-types': r.secondaryTypes ?? [],
      'first-release-date': r.date,
      disambiguation: '',
    })),
  };
}

function releaseWithRecordingsJson(release: FixtureRelease): Record<string, unknown> {
  return {
    ...releaseJson(release),
    'label-info': [],
    media: [
      {
        position: 1,
        format: 'Digital Media',
        title: '',
        'track-count': release.tracks.length,
        'track-offset': 0,
        tracks: release.tracks.map((track) => ({
          id: `00000000-0000-4000-8000-${track.mbid.slice(0, 12)}`,
          position: track.position,
          number: String(track.position),
          title: track.title,
          length: track.lengthMs,
          'artist-credit': artistCreditJson(release),
          recording: {
            id: track.mbid,
            title: track.title,
            length: track.lengthMs,
            video: false,
            disambiguation: '',
            'first-release-date': release.date,
            'artist-credit': artistCreditJson(release),
            aliases: [],
            isrcs: [],
            relations: [],
          },
        })),
      },
    ],
  };
}

// ---- server -----------------------------------------------------------------

/**
 * Applies the control to beets requests: logs them, waits the latency, then
 * answers 503 when unavailable. The handler gets whether to weaken what it
 * serves.
 */
function served(
  control: MbControl,
  handler: (ctx: RouteContext, weak: boolean) => void,
): (ctx: RouteContext) => Promise<void> {
  return async (ctx) => {
    if (!isBeets(ctx.req)) {
      handler(ctx, false);
      return;
    }
    control.requests.push(ctx.url.pathname);
    if (control.latencyMs > 0) {
      await sleep(control.latencyMs);
    }
    if (control.mode === 'unavailable') {
      console.log(`[mb] unavailable: 503 for ${ctx.url.pathname}`);
      sendJson(ctx.res, 503, { error: 'Service Temporarily Unavailable' });
      return;
    }
    handler(ctx, control.mode === 'weak');
  };
}

export function createMusicBrainzServer(control: MbControl): Server {
  return createStubServer({
    name: 'musicbrainz',
    routes: [
      {
        method: 'GET',
        pattern: '/ws/2/recording',
        handler: served(control, ({ res, url }, weak) => {
          const query = url.searchParams.get('query') ?? '';
          const limitParam = Number(url.searchParams.get('limit') ?? '25');
          const limit = Number.isFinite(limitParam) && limitParam > 0 ? limitParam : 25;
          const hits = searchRecordings(query)
            .sort((a, b) => b.release.rating - a.release.rating)
            .slice(0, limit)
            .map((hit) => (weak ? weakHit(hit) : hit));
          console.log(`[mb] recording search "${query}" -> ${hits.length}`);
          sendJson(res, 200, {
            created: new Date().toISOString(),
            count: hits.length,
            offset: 0,
            recordings: hits.map((hit) => recordingJson(hit, true)),
          });
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/recording/:id',
        handler: served(control, ({ res, params }, weak) => {
          const hit = findRecording(params.id as string);
          console.log(`[mb] recording lookup ${params.id} -> ${hit ? 'hit' : 'miss'}`);
          if (!hit) {
            sendJson(res, 404, { error: 'Not Found' });
            return;
          }
          sendJson(res, 200, recordingJson(weak ? weakHit(hit) : hit, true));
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/release-group',
        handler: served(control, ({ res, url }) => {
          if (url.searchParams.has('artist')) {
            sendJson(res, 200, browseReleaseGroups(control, url));
            return;
          }
          const query = url.searchParams.get('query') ?? '';
          const found = searchReleaseGroups(query).sort((a, b) => b.rating - a.rating);
          console.log(`[mb] release-group search "${query}" -> ${found.length}`);
          sendJson(res, 200, {
            created: new Date().toISOString(),
            count: found.length,
            offset: 0,
            'release-groups': found.map((r) => releaseGroupJson(r)),
          });
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/release-group/:id',
        handler: served(control, ({ res, params }) => {
          const id = params.id as string;
          const group = catalog(control).filter((r) => r.releaseGroupMbid === id);
          console.log(`[mb] release-group lookup ${id} -> ${group.length > 0 ? 'hit' : 'miss'}`);
          const first = group[0];
          if (!first) {
            sendJson(res, 404, { error: 'Not Found' });
            return;
          }
          sendJson(res, 200, { ...releaseGroupJson(first), releases: group.map(releaseJson) });
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/artist',
        handler: served(control, ({ res, url }) => {
          const query = url.searchParams.get('query') ?? '';
          const limitParam = Number(url.searchParams.get('limit') ?? '25');
          const limit = Number.isFinite(limitParam) && limitParam > 0 ? limitParam : 25;
          const found = searchArtists(control, query);
          console.log(`[mb] artist search "${query}" -> ${found.length}`);
          sendJson(res, 200, {
            created: new Date().toISOString(),
            count: found.length,
            offset: 0,
            artists: found.slice(0, limit).map(artistJson),
          });
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/artist/:id',
        handler: served(control, ({ res, params }) => {
          const artist = artists(control).find((a) => a.id === params.id);
          console.log(`[mb] artist lookup ${params.id} -> ${artist ? 'hit' : 'miss'}`);
          if (!artist) {
            sendJson(res, 404, { error: 'Not Found' });
            return;
          }
          sendJson(res, 200, artistJson(artist));
        }),
      },
      {
        method: 'GET',
        pattern: '/ws/2/release/:id',
        handler: served(control, ({ res, params }, weak) => {
          const release =
            findRelease(params.id as string) ??
            control.added.find((r) => r.releaseMbid === params.id);
          console.log(`[mb] release lookup ${params.id} -> ${release ? 'hit' : 'miss'}`);
          if (!release) {
            sendJson(res, 404, { error: 'Not Found' });
            return;
          }
          sendJson(res, 200, releaseWithRecordingsJson(weak ? weakened(release) : release));
        }),
      },

      // ---- control API (specs only, not part of MusicBrainz) ----------------
      {
        method: 'POST',
        pattern: '/_control/mode',
        handler: ({ res, body }) => {
          const request = (body ?? {}) as { mode?: unknown; latencyMs?: unknown };
          const mode = request.mode as MbMode | undefined;
          if (mode !== undefined && !MB_MODES.includes(mode)) {
            sendJson(res, 400, { error: `mode must be one of ${MB_MODES.join(', ')}` });
            return;
          }
          const latencyMs = request.latencyMs;
          if (
            latencyMs !== undefined &&
            (typeof latencyMs !== 'number' || !Number.isFinite(latencyMs) || latencyMs < 0)
          ) {
            sendJson(res, 400, { error: 'latencyMs must be a number of milliseconds >= 0' });
            return;
          }
          control.mode = mode ?? control.mode;
          control.latencyMs = latencyMs ?? control.latencyMs;
          console.log(`[mb] mode ${control.mode}, beets latency ${control.latencyMs} ms`);
          sendJson(res, 200, { ok: true, mode: control.mode, latencyMs: control.latencyMs });
        },
      },
      {
        method: 'GET',
        pattern: '/_control/requests',
        handler: ({ res }) => {
          sendJson(res, 200, control.requests);
        },
      },
      {
        // Simulates "a new release group appeared" for an artist.
        method: 'POST',
        pattern: '/_control/release-group',
        handler: ({ res, body }) => {
          const request = (body ?? {}) as Record<string, unknown>;
          const title = request.title;
          if (typeof title !== 'string' || title.length === 0) {
            sendJson(res, 400, { error: 'title is required' });
            return;
          }
          const artistMbid =
            typeof request.artistMbid === 'string' ? request.artistMbid : monitorArtistMbid;
          const artist = artists(control).find((a) => a.id === artistMbid);
          if (!artist) {
            sendJson(res, 400, { error: `unknown artistMbid ${artistMbid}` });
            return;
          }
          const primaryType = (request.primaryType ?? 'Album') as FixtureRelease['primaryType'];
          if (!['Album', 'EP', 'Single'].includes(primaryType)) {
            sendJson(res, 400, { error: 'primaryType must be Album, EP or Single' });
            return;
          }
          const n = control.added.length + 1;
          const hex = (value: number, width: number): string =>
            value.toString(16).padStart(width, '0');
          const release: FixtureRelease = {
            releaseMbid:
              typeof request.releaseMbid === 'string'
                ? request.releaseMbid
                : `${hex(0xadde0000 + n, 8)}-aaaa-4a5b-8c6d-7e8f9a0b1c2d`,
            releaseGroupMbid:
              typeof request.releaseGroupMbid === 'string'
                ? request.releaseGroupMbid
                : `${hex(0xadde1000 + n, 8)}-bbbb-4a5b-8c6d-7e8f9a0b1c2d`,
            title,
            artist: artist.name,
            artistMbid: artist.id,
            date: typeof request.date === 'string' ? request.date : '2024-01-01',
            primaryType,
            secondaryTypes: Array.isArray(request.secondaryTypes)
              ? request.secondaryTypes.filter((t): t is string => typeof t === 'string')
              : [],
            rating: 50,
            tracks: [1, 2].map((position) => ({
              mbid: `${hex(0xadde2000 + n * 10 + position, 8)}-cccc-4a5b-8c6d-7e8f9a0b1c2d`,
              title: `${title} Track ${position}`,
              position,
              lengthMs: 30000,
            })),
          };
          control.added.push(release);
          console.log(`[mb] added release group "${title}" to ${artist.name}`);
          sendJson(res, 200, {
            ok: true,
            releaseGroupMbid: release.releaseGroupMbid,
            releaseMbid: release.releaseMbid,
          });
        },
      },
      {
        method: 'POST',
        pattern: '/_control/reset',
        handler: ({ res }) => {
          control.reset();
          sendJson(res, 200, { ok: true });
        },
      },
    ],
  });
}
