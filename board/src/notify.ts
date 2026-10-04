import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

let granted: boolean | null = null;

/** Shows an OS notification, requesting permission on first use. Failures are ignored. */
export const notify = async (title: string, body: string): Promise<void> => {
  try {
    if (granted === null) {
      granted = (await isPermissionGranted()) || (await requestPermission()) === "granted";
    }
    if (granted) sendNotification({ title, body });
  } catch {
    // Notifications are best effort.
  }
};
