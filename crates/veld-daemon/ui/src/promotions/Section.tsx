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

import { Glyph } from "./Brand";
import { formatDay, type Section } from "./model";

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
}) {
  const s = props.section;
  return (
    <article className="promo-section">
      <div className="promo-section-lede">
        <span className="promo-section-glyph">
          <Glyph name={s.glyph} size={18} />
        </span>
        <span className="promo-section-eyebrow">{s.eyebrow}</span>
        {props.day && <time className="promo-section-day">{formatDay(props.day)}</time>}
      </div>
      <h3 className="promo-section-headline">{s.headline}</h3>
      <p className="promo-section-body">{s.body}</p>
    </article>
  );
}
