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
import type { Meeting } from "@/lib/api";

const PROVIDER: Record<Meeting["provider"], string> = { google: "Google Calendar", microsoft: "Outlook" };

/** Ask mode: the calendar meeting has started and the user joined the call. */
export function MeetingPrompt({
  meeting,
  onRecord,
  onDismiss,
}: {
  meeting: Meeting | null;
  onRecord: () => void;
  onDismiss: () => void;
}) {
  return (
    <AlertDialog open={!!meeting} onOpenChange={(open) => !open && onDismiss()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Record “{meeting?.title}”?</AlertDialogTitle>
          <AlertDialogDescription>
            This meeting from {meeting ? PROVIDER[meeting.provider] : "your calendar"} has started and your microphone
            is in use. Recording stops when you leave the call.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Not this one</AlertDialogCancel>
          <AlertDialogAction onClick={onRecord}>
            <span className="size-2.5 rounded-full bg-tally" aria-hidden />
            Record meeting
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
