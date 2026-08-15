export type BoardCardPosition = [cardId: number, position: number];

export interface BoardCardOrder<TCard> {
	cards: TCard[];
	positions: BoardCardPosition[];
}

interface PublishBoardCardOrderOptions<TCard extends { id: number }> {
	cards: readonly TCard[];
	cardId: number;
	targetIndex: number;
	optimisticUpdate: (cards: TCard[]) => void;
	publish: (positions: BoardCardPosition[]) => Promise<unknown>;
	reload: () => Promise<void>;
}

/**
 * Move one card to its final zero-based index without rebuilding the card.
 * Keeping the original objects is important: ordering must not discard an
 * issue link, note, or any metadata added to the board response later.
 */
export function reorderBoardCards<TCard extends { id: number }>(
	cards: readonly TCard[],
	cardId: number,
	targetIndex: number,
): BoardCardOrder<TCard> | null {
	const sourceIndex = cards.findIndex((card) => card.id === cardId);
	if (sourceIndex < 0 || cards.length < 2) return null;

	const boundedTarget = Math.max(0, Math.min(targetIndex, cards.length - 1));
	if (sourceIndex === boundedTarget) return null;

	const reordered = [...cards];
	const [card] = reordered.splice(sourceIndex, 1);
	reordered.splice(boundedTarget, 0, card);

	return {
		cards: reordered,
		positions: reordered.map((entry, position) => [entry.id, position]),
	};
}

/**
 * Publish an optimistic same-column reorder and reconcile it with a fresh
 * board response. A rejected publication also reloads the board so the UI
 * cannot remain in an order the server never accepted.
 */
export async function publishBoardCardOrder<TCard extends { id: number }>({
	cards,
	cardId,
	targetIndex,
	optimisticUpdate,
	publish,
	reload,
}: PublishBoardCardOrderOptions<TCard>): Promise<boolean> {
	const next = reorderBoardCards(cards, cardId, targetIndex);
	if (!next) return false;

	optimisticUpdate(next.cards);
	try {
		await publish(next.positions);
	} catch (publishError) {
		try {
			await reload();
		} catch (reloadError) {
			throw new AggregateError(
				[publishError, reloadError],
				'Failed to reorder cards and reload the board',
			);
		}
		throw publishError;
	}

	await reload();
	return true;
}
