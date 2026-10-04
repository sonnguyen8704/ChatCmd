import { fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../api';
import { setAppLanguage } from '../i18n';
import { UpdateSettings } from '../updates/UpdateSettings';
import { updateCopy } from '../updates/copy';
import type { UpdateStatus } from '../updates/types';

const available: UpdateStatus = {
  currentVersion: '1.0', latestVersion: '2.0', releaseTag: 'v2.0',
  releaseName: 'Release 2', releaseUrl: null, note: null,
  platform: 'macos', architecture: 'aarch64', debugBuild: false,
  phase: 'available', updateAvailable: true, downloadAvailable: true,
  assetName: 'ChatCMD-macos-apple-silicon.zip', checksumVerified: false,
  progressPercent: null, downloadedBytes: 0, totalBytes: null, message: null,
};

beforeEach(() => {
  setAppLanguage('en', false);
  vi.spyOn(api, 'updateStatus').mockResolvedValue(available);
  vi.spyOn(api, 'checkForUpdate').mockResolvedValue(available);
  vi.spyOn(api, 'startUpdate').mockResolvedValue({ ...available, phase: 'readyToRestart' });
  vi.spyOn(api, 'restartForUpdate').mockRejectedValue(new Error('installer unavailable'));
});
afterEach(() => { vi.restoreAllMocks(); });

describe('update restart controls', () => {
  it('offers a manual restart when opening an already prepared update', async () => {
    vi.mocked(api.updateStatus).mockResolvedValue({ ...available, phase: 'readyToRestart' });
    render(<UpdateSettings />);
    expect(await screen.findByRole('button', { name: updateCopy().restart })).toBeEnabled();
    expect(api.restartForUpdate).not.toHaveBeenCalled();
  });

  it('automatically restarts a confirmed update and restores manual retry on failure', async () => {
    render(<UpdateSettings />);
    fireEvent.click(await screen.findByRole('button', { name: updateCopy().update }));
    fireEvent.click(within(screen.getByRole('alertdialog')).getByRole('button', { name: updateCopy().confirmUpdate }));
    expect(await screen.findByText('installer unavailable')).toBeVisible();
    expect(api.startUpdate).toHaveBeenCalledOnce();
    expect(api.restartForUpdate).toHaveBeenCalledOnce();
    const retry = screen.getByRole('button', { name: updateCopy().restart });
    expect(retry).toBeEnabled();
    fireEvent.click(retry);
    await screen.findByText('installer unavailable');
    expect(api.restartForUpdate).toHaveBeenCalledTimes(2);
  });
});
