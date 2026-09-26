import type { AnswerOutcome, MutationAck } from "../wire/types";

export interface OutcomeCopy {
  text: string;
  /** Nothing more to do on this sheet. */
  final: boolean;
  /** The row is answered on the daemon and leaves the open list. */
  retire: boolean;
}

/** One line of copy per outcome the daemon can hand back for an answer (D18). */
export function outcomeCopy(outcome: AnswerOutcome, ack?: MutationAck): OutcomeCopy {
  switch (outcome.kind) {
    case "delivered":
      return { text: ack?.receipt === "delivered" || !ack ? "Delivered" : `Delivered (${ack.receipt})`, final: true, retire: true };
    case "already_answered":
      return { text: `Already answered by ${outcome.by}`, final: true, retire: true };
    case "ambiguous":
      return { text: "Could not confirm, check the session", final: true, retire: false };
    case "no_target":
      return { text: "No live session to answer", final: true, retire: false };
    case "delivery_failed":
      // The row is answered; a retry would only replay this. The person looks.
      return { text: "Answered but not delivered, check the session", final: true, retire: true };
    case "rejected":
      return { text: rejectedCopy(outcome.reason), final: true, retire: outcome.reason === "already_answered_by" };
    case "unknown":
      return { text: "Could not confirm, check the session", final: true, retire: false };
  }
}

function rejectedCopy(reason: string): string {
  switch (reason) {
    case "already_answered_by":
      return "Already answered elsewhere";
    case "op_expired":
      return "Too old to retry, reopen the question";
    case "turn_advanced":
      return "The agent moved on";
    case "effects_ambiguous":
      return "Could not confirm, check the session";
    default:
      return `Refused: ${reason}`;
  }
}
