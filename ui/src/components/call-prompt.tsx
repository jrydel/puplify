import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import type { Call } from "@/lib/api";
import { CALENDAR_NAME } from "@/lib/format";

/** Ask mode: the user joined a call, from the calendar or not. */
export function CallPrompt({
  call,
  onRecord,
  onDismiss,
}: {
  call: Call | null;
  onRecord: () => void;
  onDismiss: () => void;
}) {
  return (
    <AlertDialog open={!!call} onOpenChange={(open) => !open && onDismiss()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Record “{call?.title}”?</AlertDialogTitle>
          <AlertDialogDescription>
            {call?.meeting
              ? `This meeting from ${CALENDAR_NAME[call.meeting.provider]} has started and your microphone is in use.`
              : `${call?.app ?? "Another app"} is using your microphone, so you seem to be in a call.`}{" "}
            Recording stops when you leave the call.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Not this one</AlertDialogCancel>
          <AlertDialogAction onClick={onRecord}>
            <span className="size-2.5 rounded-full bg-tally" aria-hidden />
            Record call
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
