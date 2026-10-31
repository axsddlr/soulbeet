// Manual source selection: "Search sources" opens the download options view
// where the user inspects candidate groups, picks tracks and a target folder.

import { expect, test } from '@playwright/test';
import { freshSession, openSettings, performSearch } from '../helpers/app.js';
import { expectFileAppears, importedTrackPath } from '../helpers/files.js';
import { glassAtlas, maritime } from '../fixtures/dataset.js';

test.describe.configure({ timeout: 180_000 });

test('lists scored sources and downloads a hand-picked track', async ({ context, page }) => {
  const { folder } = await freshSession(context, page, 'manual-track');

  await performSearch(page, maritime.tracks[0]!.title, 'TRACK');

  const row = page.locator('li').filter({ hasText: maritime.tracks[0]!.title }).first();
  await row.getByRole('button', { name: 'Search sources' }).click();

  // The options view shows the scored groups; the top one carries the badge
  // and the lossless quality details (bit depth / sample rate).
  await expect(page.getByText('Download Options')).toBeVisible();
  await expect(page.getByText('Best match')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText('FLAC 16/44.1').first()).toBeVisible();

  await page.getByRole('button', { name: 'Select All' }).first().click();
  await page.locator('select[name="dl_folder"]').selectOption({ label: folder.name });
  await page.getByRole('button', { name: 'Start download' }).click();

  const imported = importedTrackPath(
    folder.name,
    maritime.artist,
    maritime.title,
    maritime.tracks[0]!.title,
    'flac',
  );
  await expectFileAppears(imported, 120_000);
});

test('returns to search results from the options view', async ({ context, page }) => {
  await freshSession(context, page, 'manual-back');

  await performSearch(page, glassAtlas.title, 'ALBUM');

  const card = page.locator('li').filter({ hasText: glassAtlas.title }).first();
  await card.getByRole('button', { name: 'Search sources' }).click();
  await expect(page.getByText('Download Options')).toBeVisible();

  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page.getByText('Search Results')).toBeVisible();
});

test('Require FLAC only keeps mp3 sources out of the options', async ({ context, page }) => {
  await freshSession(context, page, 'manual-flac-only');

  // Glass Atlas is shared as FLAC by collector_01 and as mp3 by mp3_hoarder.
  const openOptions = async () => {
    await performSearch(page, glassAtlas.title, 'ALBUM');
    const card = page.locator('li').filter({ hasText: glassAtlas.title }).first();
    await card.getByRole('button', { name: 'Search sources' }).click();
    await expect(page.getByText('Download Options')).toBeVisible();
    await expect(page.getByText(/^Searching\.\.\./)).toBeHidden({ timeout: 90_000 });
  };

  // Without the setting both formats are offered.
  await openOptions();
  await expect(page.getByText(/Quality: mp3/).first()).toBeVisible();

  await openSettings(page);
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  const toggle = page.getByRole('switch', { name: 'Require FLAC only' });
  await expect(toggle).toHaveAttribute('aria-checked', 'false');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-checked', 'true');

  // The saved value survives a full reload.
  await page.reload();
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByRole('switch', { name: 'Require FLAC only' })).toHaveAttribute(
    'aria-checked',
    'true',
  );

  await page.goto('/');
  await openOptions();
  await expect(page.getByText(/Quality: flac/).first()).toBeVisible();
  await expect(page.getByText(/Quality: mp3/)).toHaveCount(0);
});
