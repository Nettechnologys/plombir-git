import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

// Nothing links here: the repository's Board tab opens `/boards`, and this
// older copy of the board page had drifted from it — no translations, its own
// mutation code (card_c30077df5603). An old bookmark lands on the live page.
export const load: PageLoad = ({ params }) => {
  redirect(308, `/${params.owner}/${params.repo}/boards`);
};
