import { useEffect, useRef, useState } from "react";
import { CopyIcon, SendIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";

import { DeliveryList } from "@/components/delivery-list";
import { TapeStrip } from "@/components/tape-strip";
import { TranscriptBody, type TranscriptBodyHandle } from "@/components/transcript-body";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { api, errorText, type DeliveryResult, type Transcript } from "@/lib/api";
import { fmtDuration } from "@/lib/format";

interface TranscriptViewProps {
  id: string;
  onChanged: () => void;
  onDeleted: () => void;
}

export function TranscriptView({ id, onChanged, onDeleted }: TranscriptViewProps) {
  const [data, setData] = useState<{ transcript: Transcript; text: string } | null>(null);
  const [title, setTitle] = useState("");
  const [sending, setSending] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deliveries, setDeliveries] = useState<DeliveryResult[]>([]);
  const body = useRef<TranscriptBodyHandle>(null);

  useEffect(() => {
    let cancelled = false;
    setData(null);
    setDeliveries([]);
    api
      .getTranscript(id)
      .then((d) => {
        if (cancelled) return;
        setData(d);
        setTitle(d.transcript.title);
      })
      .catch((e) => toast.error(errorText(e)));
    return () => {
      cancelled = true;
    };
  }, [id]);

  if (!data) {
    return (
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-4 pt-2">
        <Skeleton className="h-9 w-2/3" />
        <Skeleton className="h-4 w-1/3" />
        <Skeleton className="mt-4 h-16 w-full" />
      </div>
    );
  }

  const t = data.transcript;
  const started = new Date(t.started_at);
  const durationMs = t.ended_at ? new Date(t.ended_at).getTime() - started.getTime() : 0;

  async function rename() {
    const next = title.trim();
    if (!next || next === t.title) {
      setTitle(t.title);
      return;
    }
    try {
      await api.renameTranscript(id, next);
      setData(await api.getTranscript(id));
      onChanged();
    } catch (e) {
      toast.error(errorText(e));
      setTitle(t.title);
    }
  }

  async function send() {
    setSending(true);
    setDeliveries([]);
    try {
      const results = await api.deliverTranscript(id);
      setDeliveries(results);
      const failed = results.filter((r) => !r.ok).length;
      if (failed) toast.error(`${failed} of ${results.length} destinations failed`);
      else toast.success(`Sent to ${results.length} ${results.length === 1 ? "destination" : "destinations"}`);
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setSending(false);
    }
  }

  /**
   * The dialog stays open (blurred backdrop and all) until the file is gone;
   * the parent then drops the sidebar row and leaves this page in one render,
   * so the screen changes once instead of in three steps.
   */
  async function remove() {
    setDeleting(true);
    try {
      await api.deleteTranscript(id);
      onDeleted();
      toast.success("Transcript deleted");
    } catch (e) {
      toast.error(errorText(e));
      setDeleting(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
      <header className="flex flex-col gap-3">
        <input
          aria-label="Transcript title"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={rename}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") {
              setTitle(t.title);
              e.currentTarget.blur();
            }
          }}
          className="w-full rounded-md bg-transparent font-heading text-3xl font-semibold tracking-tight outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
          <span>{started.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</span>
          <span aria-hidden>·</span>
          <span>{fmtDuration(durationMs)}</span>
          <Badge variant="secondary" className="font-mono">
            {t.engine}
          </Badge>
          <div className="ml-auto flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                navigator.clipboard
                  .writeText(data.text)
                  .then(() => toast.success("Transcript copied"))
                  .catch((e) => toast.error(`Couldn't copy: ${errorText(e)}`))
              }
            >
              <CopyIcon data-icon="inline-start" />
              Copy text
            </Button>
            <Button variant="outline" size="sm" onClick={send} disabled={sending}>
              {sending ? <Spinner data-icon="inline-start" /> : <SendIcon data-icon="inline-start" />}
              Send again
            </Button>
            <AlertDialog open={confirmDelete} onOpenChange={(open) => !deleting && setConfirmDelete(open)}>
              <AlertDialogTrigger asChild>
                <Button variant="ghost" size="sm" aria-label="Delete transcript">
                  <Trash2Icon />
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete “{t.title}”?</AlertDialogTitle>
                  <AlertDialogDescription>
                    This removes the transcript from this computer. Copies already sent to OneDrive or other
                    destinations stay where they are.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel disabled={deleting}>Keep it</AlertDialogCancel>
                  <AlertDialogAction
                    variant="destructive"
                    disabled={deleting}
                    onClick={(e) => {
                      e.preventDefault(); // close only after the delete, in the same render as leaving
                      remove();
                    }}
                  >
                    {deleting && <Spinner data-icon="inline-start" />}
                    Delete transcript
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </div>
        <DeliveryList results={deliveries} />
      </header>

      {t.segments.length > 0 && (
        <TapeStrip
          segments={t.segments}
          durationMs={durationMs}
          labels={t.labels}
          onSeek={(i) => body.current?.reveal(i)}
        />
      )}

      <Separator />

      {t.segments.length === 0 ? (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>Nothing was transcribed</EmptyTitle>
            <EmptyDescription>No speech was detected in this recording.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <TranscriptBody ref={body} segments={t.segments} labels={t.labels} />
      )}
    </div>
  );
}
