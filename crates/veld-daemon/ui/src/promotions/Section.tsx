/**
 * The one content atom, rendered.
 *
 * This component takes a `Section` and nothing else — no size, no variant, no
 * layout. What differs between the start screen and the what's-new panel is the
 * *container* around this, expressed in CSS (`.promo-grid` vs `.promo-stack`),
 * which is what "two layouts composing one atom" has to mean if it is to mean
 * anything. The moment a prop here selects between two renderings, content and
 * surface are coupled again and every future surface is another branch.
 *
 * Three rows, in reading order: the glyph sits *beside the eyebrow* rather than
 * in a gutter down the left, so the headline and the sentence both start at the
 * card's edge and share one measure. A gutter costs the two longest lines about
 * three characters each and buys nothing.
 */

import { Glyph, Logomark } from "./Brand";
import { formatDay, type Section, type Source, sourceByline } from "./model";

/**
 * Whose card this is.
 *
 * **A trust requirement, not a credit line.** A project's card is text a teammate
 * wrote, rendered in a page same-origin with the daemon API and with terminal
 * tickets, so it must never be mistakable for something Veld said. The two get
 * deliberately different *kinds* of mark rather than the same chip with different
 * words in it: Veld's is the `V.` icon mark, which no repo can produce, and a
 * project's is a bordered pill, which reads as a label attached to content rather
 * than as an endorsement of it.
 *
 * A repo *can* of course call itself `veld` — the pill would then name `veld` —
 * which is why the distinction is carried by the mark's form and not by its text.
 *
 * Both bylines say what they mean in full, rather than in one decodable word:
 * "Official veld news" and "News from your project – ‹name›". A bare "Official"
 * beside a bare name only works for a reader who already knows what the two
 * alternatives are, which is exactly the reader this does not need to convince.
 */
function SourceMark(props: { source: Source }) {
  if (props.source.kind === "veld") {
    return (
      <span className="promo-section-source-veld">
        {/* The mark, which no repo can produce, then the claim it stands for. The
            icon rather than the wordmark: a byline is one 10.5px line, and `veld.`
            at that height is four letterforms competing with the sentence above
            them. The mark alone was too quiet to read as a claim at all, and —
            measured on this repo, which is itself named `veld` — a name alone
            cannot distinguish the two. */}
        <Logomark size={13} />
        <span className="promo-section-source-sep" aria-hidden="true">
          –
        </span>
        <span className="promo-section-official">{sourceByline(props.source)}</span>
      </span>
    );
  }
  return (
    <span className="promo-section-project">
      {sourceByline(props.source)}
      <span className="promo-section-source-sep" aria-hidden="true">
        –
      </span>
      {/* The name last and on its own, so it carries the weight, truncates by
          itself on a long repo name, and cannot be written into the middle of the
          claim by a repo that names itself after part of it. */}
      <span className="promo-section-project-name">{props.source.name}</span>
    </span>
  );
}

export function PromoSection(props: {
  section: Section;
  /**
   * The day this shipped, `YYYY-MM-DD`, when the surface has one to show.
   *
   * A prop rather than a field on `Section`, because the two collections differ:
   * a promotion landed on a day and saying so helps a reader catch up, while the
   * three start-screen claims are what Veld *is* and dating them would be odd.
   * The surface decides whether to pass it; the atom just renders what it got.
   */
  day?: string;
  /**
   * Who wrote this, when the surface has more than one possible answer.
   *
   * A prop for the same reason `day` is one: the start screen's three claims are
   * Veld's by construction and attributing them would be noise, while a panel
   * that mixes Veld's cards with a project's cannot leave the question open.
   */
  source?: Source;
}) {
  const s = props.section;
  /**
   * A project's card **loses the brand accent** — the green glyph tile and the
   * green eyebrow — and keeps a neutral treatment instead.
   *
   * This is the distinction doing the work that a byline could not. Green is the
   * app's own colour, so painting repo-authored text in it is the mistakability
   * this attribution exists to prevent, and a small mark under the sentence is
   * not enough to undo it: at a glance every card read as Veld's. Colour is
   * legible before any text is, which is exactly the property needed here.
   */
  const project = props.source?.kind === "project";
  return (
    <article className={project ? "promo-section promo-section-from-project" : "promo-section"}>
      <div className="promo-section-lede">
        <span className="promo-section-glyph">
          <Glyph name={s.glyph} size={18} />
        </span>
        <span className="promo-section-eyebrow">{s.eyebrow}</span>
        {props.day && <time className="promo-section-day">{formatDay(props.day)}</time>}
      </div>
      <h3 className="promo-section-headline">{s.headline}</h3>
      <p className="promo-section-body">{s.body}</p>
      {/* Under the sentence rather than up in the lede: that row already carries
          the eyebrow and the date, and a third thing in it is the crowding that
          makes all three harder to read. Attribution is what you look for after
          reading, not before. */}
      {props.source && (
        <div className="promo-section-source">
          <SourceMark source={props.source} />
        </div>
      )}
    </article>
  );
}
