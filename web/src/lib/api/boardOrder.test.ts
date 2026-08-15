import { describe, expect, it, vi } from 'vitest';

import { publishBoardCardOrder, reorderBoardCards } from './boardOrder';

interface TestCard {
	id: number;
	note: string;
	issue_id: number | null;
}

const cards: TestCard[] = [
	{ id: 1, note: 'linked', issue_id: 42 },
	{ id: 2, note: 'standalone', issue_id: null },
	{ id: 3, note: 'last', issue_id: null },
];

describe('board card ordering', () => {
	it('builds the full order while preserving card contents', () => {
		const result = reorderBoardCards(cards, 3, 0);

		expect(result?.positions).toEqual([
			[3, 0],
			[1, 1],
			[2, 2],
		]);
		expect(result?.cards).toEqual([cards[2], cards[0], cards[1]]);
		expect(result?.cards[1]).toMatchObject({ note: 'linked', issue_id: 42 });
	});

	it('publishes the optimistic order and replaces it with the reread order', async () => {
		let serverCards = [...cards];
		let visibleCards = [...cards];
		const reload = vi.fn(async () => {
			visibleCards = serverCards.map((card) => ({ ...card }));
		});

		await publishBoardCardOrder({
			cards: visibleCards,
			cardId: 3,
			targetIndex: 0,
			optimisticUpdate: (next) => {
				visibleCards = next;
			},
			publish: async (positions) => {
				serverCards = positions.map(([id]) => serverCards.find((card) => card.id === id)!);
			},
			reload,
		});

		expect(visibleCards.map((card) => card.id)).toEqual([3, 1, 2]);
		expect(visibleCards[1]).toMatchObject({ note: 'linked', issue_id: 42 });
		expect(reload).toHaveBeenCalledOnce();
	});

	it('reloads the accepted server order after a publication error', async () => {
		const publishError = new Error('reorder rejected');
		let visibleCards = [...cards];
		const reload = vi.fn(async () => {
			visibleCards = cards.map((card) => ({ ...card }));
		});

		await expect(
			publishBoardCardOrder({
				cards: visibleCards,
				cardId: 3,
				targetIndex: 0,
				optimisticUpdate: (next) => {
					visibleCards = next;
				},
				publish: async () => {
					throw publishError;
				},
				reload,
			}),
		).rejects.toBe(publishError);

		expect(visibleCards.map((card) => card.id)).toEqual([1, 2, 3]);
		expect(reload).toHaveBeenCalledOnce();
	});

	it('reports both failures when the rejected order cannot be reloaded', async () => {
		const publishError = new Error('reorder rejected');
		const reloadError = new Error('reload failed');

		await expect(
			publishBoardCardOrder({
				cards,
				cardId: 3,
				targetIndex: 0,
				optimisticUpdate: () => {},
				publish: async () => {
					throw publishError;
				},
				reload: async () => {
					throw reloadError;
				},
			}),
		).rejects.toMatchObject({ errors: [publishError, reloadError] });
	});
});
