import { invoke } from '@tauri-apps/api/core';

/** Whether the app runs from somewhere macOS will not let it update itself. */
export function needsMove(): Promise<boolean> {
	return invoke('plugin:move-to-applications|needs_move');
}

/**
 * Moves the app into /Applications and relaunches it from there. Resolves only
 * if the user cancels the administrator password prompt.
 */
export function moveToApplicationsFolder(): Promise<void> {
	return invoke('plugin:move-to-applications|move_to_applications_folder');
}
