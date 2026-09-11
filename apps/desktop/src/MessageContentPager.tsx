import { useEffect, useState } from "react";

import { useLocale } from "./i18n";
import type { MessageContentPage, MessageContentRequest } from "./types";
import { Button } from "./ui/primitives";

export type ReadMessageContent = (
  request: MessageContentRequest,
  signal: AbortSignal,
) => Promise<MessageContentPage>;

/** Keeps one page mounted; closing or changing pages cancels its native query owner. */
export function MessageContentPager({ displayId, onRead }: {
  readonly displayId: string;
  readonly onRead: ReadMessageContent;
}) {
  const { t } = useLocale();
  const [open, setOpen] = useState(false);
  const [request, setRequest] = useState<MessageContentRequest>({ displayId });
  const [previousOffsets, setPreviousOffsets] = useState<number[]>([]);
  const [page, setPage] = useState<MessageContentPage>();
  const [failure, setFailure] = useState(false);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setPage(undefined);
    setFailure(false);
    void onRead({ ...request, displayId, limit: 65_536 }, controller.signal).then((result) => {
      if (controller.signal.aborted) return;
      const bytes = new TextEncoder().encode(result.text).length;
      const end = result.offset + bytes;
      if (result.displayId !== displayId || result.offset !== (request.offset ?? 0)
        || (request.contentVersion !== undefined && result.contentVersion !== request.contentVersion)
        || bytes > 65_536 || end > result.totalBytes
        || (result.nextOffset === null ? end !== result.totalBytes : result.nextOffset !== end || bytes === 0)) {
        setFailure(true);
        return;
      }
      setPage(result);
    }).catch(() => {
      if (!controller.signal.aborted) setFailure(true);
    });
    return () => controller.abort();
  }, [displayId, onRead, open, request, retry]);

  return (
    <section className="message-full-content" aria-label={t("messageFullContent")}>
      <Button type="button" variant="quiet" data-message-content-toggle aria-expanded={open}
        onClick={() => {
          setOpen(!open);
          setRequest({ displayId });
          setPreviousOffsets([]);
          setPage(undefined);
        }}>
        {t(open ? "messageContentClose" : "messageFullContent")}
      </Button>
      {!open ? null : (
        <div>
          {failure ? <div role="alert">
            <span>{t("messageContentUnavailable")}</span>
            <Button type="button" variant="quiet" onClick={() => setRetry((value) => value + 1)}>
              {t("retry")}
            </Button>
          </div> : page === undefined ? <p role="status">{t("loading")}</p> : <>
            <pre className="message-content sg-bounded-content message-content-page"
              data-message-content-page data-content-offset={page.offset}
              data-content-next-offset={page.nextOffset ?? ""}>{page.text}</pre>
            <nav aria-label={t("messageContentPages")}>
              <Button type="button" variant="quiet" data-message-content-previous
                disabled={previousOffsets.length === 0} onClick={() => {
                  const offset = previousOffsets[previousOffsets.length - 1];
                  if (offset === undefined) return;
                  setPreviousOffsets((offsets) => offsets.slice(0, -1));
                  setRequest({ displayId, offset, contentVersion: page.contentVersion });
                }}>{t("messageContentPrevious")}</Button>
              <span>{t("messageContentPage", { page: previousOffsets.length + 1 })}</span>
              <Button type="button" variant="quiet" data-message-content-next
                disabled={page.nextOffset === null} onClick={() => {
                  if (page.nextOffset === null) return;
                  setPreviousOffsets((offsets) => [...offsets, page.offset]);
                  setRequest({ displayId, offset: page.nextOffset, contentVersion: page.contentVersion });
                }}>{t("messageContentNext")}</Button>
            </nav>
          </>}
        </div>
      )}
    </section>
  );
}
