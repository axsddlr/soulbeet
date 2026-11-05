// Artist monitoring through the UI: find an artist, read their discography,
// follow them, and pick up a release MusicBrainz announces afterwards.
//
// The added release group lives on the shared MusicBrainz stub, so its title
// is unique per run: a retry's baseline check files the earlier one as known.

import type { Locator, Page } from '@playwright/test';
import { expect, test } from '@playwright/test';
import { freshSession } from '../helpers/app.js';
import { addMbReleaseGroup } from '../helpers/control.js';
import { monitorArtistMbid, monitorArtistName } from '../fixtures/dataset.js';

// MusicBrainz requests are spaced out (1 req/s by design) and queue up
// behind other workers' searches.
const MB_TIMEOUT = 45_000;
// The baseline check after a follow runs in the background.
const BASELINE_TIMEOUT = 30_000;

const groupMbid = (n: number): string => `c0de100${n}-bbbb-4a5b-8c6d-7e8f9a0b1c2d`;
const firstAlbum = groupMbid(1);
const secondAlbum = groupMbid(2);
const shortEp = groupMbid(3);
const loneSingle = groupMbid(4);
const liveAlbum = groupMbid(5);
const compilation = groupMbid(6);
const futureAlbum = groupMbid(7);

function releaseCard(page: Page, rgMbid: string): Locator {
  return page.locator(`[data-testid="release-card"][data-rg="${rgMbid}"]`);
}

function newReleaseRow(page: Page, rgMbid: string): Locator {
  return page.locator(`[data-testid="new-release-row"][data-rg="${rgMbid}"]`);
}

/** Re-reads the artist page until `check` passes, while a check runs. */
async function refreshUntil(page: Page, check: () => Promise<void>): Promise<void> {
  await expect(async () => {
    await page.getByTestId('artist-refresh').click();
    await check();
  }).toPass({ timeout: BASELINE_TIMEOUT, intervals: [1_000, 2_000, 3_000] });
}

test('follows an artist and picks up a newly announced release', async ({ context, page }) => {
  await freshSession(context, page, 'artists-follow');

  // Find the artist from the Artists page.
  await page.getByTestId('nav-artists').click();
  await expect(page).toHaveURL(/\/artists$/);
  await page.getByTestId('artist-search-input').fill('monitor');
  const result = page.locator(`[data-testid="artist-result"][data-mbid="${monitorArtistMbid}"]`);
  await expect(result).toBeVisible({ timeout: MB_TIMEOUT });
  await expect(result).toContainText(monitorArtistName);
  await result.click();

  // The discography: studio albums (the announced one too), the EP and the
  // single; live albums and compilations stay out.
  await expect(page).toHaveURL(new RegExp(`/artist/${monitorArtistMbid}$`));
  await expect(page.getByTestId('artist-name')).toHaveText(monitorArtistName, {
    timeout: MB_TIMEOUT,
  });
  const sections = page.getByTestId('discography-section');
  await expect(sections).toHaveCount(3);
  await expect(sections.nth(0)).toHaveAttribute('data-kind', 'Album');
  await expect(sections.nth(1)).toHaveAttribute('data-kind', 'EP');
  await expect(sections.nth(2)).toHaveAttribute('data-kind', 'Single');
  for (const rg of [firstAlbum, secondAlbum, futureAlbum]) {
    await expect(sections.nth(0).locator(`[data-rg="${rg}"]`)).toBeVisible();
  }
  await expect(sections.nth(1).locator(`[data-rg="${shortEp}"]`)).toContainText(
    'Monitor Short EP',
  );
  await expect(sections.nth(2).locator(`[data-rg="${loneSingle}"]`)).toContainText(
    'Monitor Lone Single',
  );
  await expect(releaseCard(page, liveAlbum)).toHaveCount(0);
  await expect(releaseCard(page, compilation)).toHaveCount(0);
  await expect(releaseCard(page, firstAlbum).getByTestId('release-download')).toBeDisabled();

  // Follow. The baseline check files released albums as known and the
  // announced one as upcoming; nothing counts as new.
  const follow = page.getByTestId('follow-button');
  await follow.click();
  await expect(follow).toHaveText('Following');
  await expect(follow).toHaveAttribute('aria-pressed', 'true');
  await refreshUntil(page, async () => {
    await expect(releaseCard(page, futureAlbum).getByTestId('status-chip')).toHaveText(
      'Upcoming',
      { timeout: 2_000 },
    );
  });
  await expect(page.locator('[data-testid="status-chip"][data-status="new"]')).toHaveCount(0);

  // MusicBrainz announces a release; a manual check finds it.
  const title = `Brand New ${Date.now()}`;
  const added = await addMbReleaseGroup({ title, date: '2025-01-01' });
  await page.getByTestId('check-now').click();
  await expect(page.getByTestId('check-result')).toContainText('1 new', { timeout: MB_TIMEOUT });
  await expect(releaseCard(page, added.releaseGroupMbid).getByTestId('status-chip')).toHaveText(
    'New',
  );

  // It is listed under New releases, then dismissed.
  await page.getByTestId('nav-artists').click();
  await expect(page.getByTestId('tab-new-releases')).toHaveAttribute('aria-selected', 'true');
  const row = newReleaseRow(page, added.releaseGroupMbid);
  await expect(row).toContainText(title);
  await expect(row).toContainText(monitorArtistName);
  await expect(row.getByTestId('status-chip')).toHaveText('New');
  await expect(page.getByTestId('nav-artists-badge').filter({ visible: true })).toHaveText('1');
  await expect(newReleaseRow(page, futureAlbum).getByTestId('status-chip')).toHaveText(
    'Upcoming',
  );
  await row.getByTestId('release-dismiss').click();
  await expect(row).toHaveCount(0);
  await expect(page.getByTestId('nav-artists-badge')).toHaveCount(0);

  // The artist is in the Following list; unfollow from their page.
  await page.getByTestId('tab-following').click();
  const followed = page.locator(
    `[data-testid="followed-artist"][data-mbid="${monitorArtistMbid}"]`,
  );
  await expect(followed).toContainText(monitorArtistName);
  await expect(followed).toContainText('Auto-download off');
  await followed.getByRole('link', { name: monitorArtistName }).click();
  await expect(follow).toHaveText('Following', { timeout: MB_TIMEOUT });
  await follow.click();
  await expect(follow).toHaveText('Follow');
  await expect(follow).toHaveAttribute('aria-pressed', 'false');
  await expect(page.getByTestId('check-now')).toHaveCount(0);

  await page.getByTestId('nav-artists').click();
  await page.getByTestId('tab-following').click();
  await expect(page.getByTestId('following-empty')).toBeVisible();
});

test('the search page links to the artists page', async ({ context, page }) => {
  await freshSession(context, page, 'artists-entry');

  await page.getByTestId('find-artists').click();
  await expect(page).toHaveURL(/\/artists$/);
  await expect(page.getByTestId('new-releases-empty')).toContainText(
    'No new releases yet. Follow artists to get notified.',
  );
});
