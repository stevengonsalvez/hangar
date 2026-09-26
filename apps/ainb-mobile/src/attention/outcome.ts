import type { AnswerOutcome, MutationAck } from "../wire/types";

/** One line of copy per outcome the daemon can hand back for an answer (D18). */
export function outcomeCopy(outcome: AnswerOutcome, ack?: MutationAck): { text: string; final: boolean } {
  switch (outcome.kind) {
    case "delivered":
      return { text: ack?.receipt === "delivered" || !ack ? "Delivered" : `Delivered (${ack.receipt})`, final: true };
    case "already_answered":
      return { text: `Already answered by ${outcome.by}`, final: true };
    case "ambiguous":
      return { text: "Could not confirm, check the session", final: true };
    case "no_target":
      return { text: "No live session to answer", final: true };
    case "delivery_failed":
      return { text: `Not delivered: ${outcome.reason}`, final: false };
    case "rejected":
      return { text: rejectedCopy(outcome.reason), final: true };
    case "unknown":
      return { text: "Could not confirm, check the session", final: true };
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
