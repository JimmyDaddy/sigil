import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { useLocale } from "../../i18n";
import { Toast, type ToastTone } from "../primitives";

export interface NotificationInput {
  readonly message: string;
  readonly title?: string;
  readonly tone?: ToastTone;
  readonly timeoutMs?: number;
}

interface NotificationEntry extends NotificationInput {
  readonly id: number;
  readonly tone: ToastTone;
  readonly timeoutMs: number;
}

interface NotificationContextValue {
  notify: (notification: NotificationInput) => number;
  dismiss: (id: number) => void;
}

const NotificationContext = createContext<NotificationContextValue | undefined>(undefined);

export function NotificationProvider({ children }: { readonly children: ReactNode }) {
  const { t } = useLocale();
  const [notifications, setNotifications] = useState<NotificationEntry[]>([]);
  const nextId = useRef(0);
  const dismiss = useCallback((id: number) => {
    setNotifications((current) => current.filter((notification) => notification.id !== id));
  }, []);
  const notify = useCallback((input: NotificationInput) => {
    const tone = input.tone ?? "info";
    const id = nextId.current += 1;
    const notification: NotificationEntry = {
      ...input,
      id,
      tone,
      timeoutMs: input.timeoutMs ?? defaultTimeout(tone),
    };
    setNotifications((current) => [
      ...current.filter((candidate) =>
        candidate.message !== notification.message || candidate.tone !== notification.tone),
      notification,
    ].slice(-4));
    return id;
  }, []);
  const value = useMemo(() => ({ notify, dismiss }), [dismiss, notify]);
  return (
    <NotificationContext.Provider value={value}>
      {children}
      {notifications.length === 0 ? null : (
        <section className="sg-notification-viewport" aria-label={t("notifications")}>
          {notifications.map((notification) => (
            <TimedNotification
              key={notification.id}
              notification={notification}
              onDismiss={dismiss}
            />
          ))}
        </section>
      )}
    </NotificationContext.Provider>
  );
}

export function useNotifications(): NotificationContextValue {
  const value = useContext(NotificationContext);
  if (value === undefined) throw new Error("useNotifications must be used inside NotificationProvider");
  return value;
}

function TimedNotification({
  notification,
  onDismiss,
}: {
  readonly notification: NotificationEntry;
  readonly onDismiss: (id: number) => void;
}) {
  const dismissCurrent = useCallback(
    () => onDismiss(notification.id),
    [notification.id, onDismiss],
  );
  useEffect(() => {
    if (notification.timeoutMs <= 0) return undefined;
    const timer = window.setTimeout(dismissCurrent, notification.timeoutMs);
    return () => window.clearTimeout(timer);
  }, [dismissCurrent, notification.timeoutMs]);
  return (
    <Toast
      title={notification.title}
      tone={notification.tone}
      urgent={notification.tone === "error"}
      timeoutMs={notification.timeoutMs > 0 ? notification.timeoutMs : undefined}
      onDismiss={dismissCurrent}
    >
      {notification.message}
    </Toast>
  );
}

function defaultTimeout(tone: ToastTone): number {
  switch (tone) {
    case "success": return 4_000;
    case "info": return 5_000;
    case "warning": return 7_000;
    case "error": return 9_000;
  }
}
