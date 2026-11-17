// Draws a batch's Merkle tree from the leaves up to the root. With `animate`, nodes appear level by level.
//
// A tree node's `leaf` is its position among the leaves that were hashed. When records were deleted
// that is no longer the slot the leaf had when the batch was sealed, so the verifier passes `slots`
// (original slot per position) and `missing` (slots with no record). Leaves are then captioned and
// placed by original slot, and each deleted slot is drawn as an empty box in its place.
import type { MerkleTreeNode } from '../api/client';

type Props = {
  tree?: MerkleTreeNode | null;
  labels?: string[];
  /** Positions (tree `leaf` values) of the leaves to flag. */
  bad?: Set<number>;
  rootBad?: boolean;
  /** Original batch slot of the leaf at each position. Defaults to the position itself. */
  slots?: number[];
  /** Original batch slots that have no leaf: deleted records. */
  missing?: number[];
  animate?: boolean;
};

const NW = 74, NH = 36, COL_W = 80, ROW_H = 66, PAD_X = 4, PAD_TOP = 4, KEY_H = 18;

const isLeaf = (n: MerkleTreeNode) => n.leaf != null;
const kids = (n: MerkleTreeNode) => [n.left, n.right].filter((c): c is MerkleTreeNode => !!c);

function Ghost({ x, y, slot }: { x: number; y: number; slot: number }) {
  return (
    <g className="t-node ghost">
      <title>{`Leaf ${slot + 1} was anchored, but its record is not in the trail`}</title>
      <rect x={x - NW / 2} y={y} width={NW} height={NH} rx={6} />
      <text className="t-cap" x={x} y={y + 13} textAnchor="middle">
        LEAF {slot + 1}
      </text>
      <text className="t-hash" x={x} y={y + 28} textAnchor="middle">
        deleted
      </text>
    </g>
  );
}

export function MerkleTree({
  tree,
  labels = [],
  bad = new Set(),
  rootBad = false,
  animate = false,
  slots,
  missing = [],
}: Props) {
  if (!tree || tree.empty) {
    const w = Math.max(200, PAD_X * 2 + NW + (missing.length - 1) * COL_W);
    const h = missing.length ? ROW_H + NH + 6 : 56;
    return (
      <div className="tree-wrap">
        <svg viewBox={`0 0 ${w} ${h}`} style={{ width: '100%', maxWidth: w, height: 'auto' }}>
          {missing.map((slot, i) => (
            <Ghost key={slot} slot={slot} x={PAD_X + NW / 2 + i * COL_W} y={ROW_H} />
          ))}
          <g className={`t-node root ${rootBad ? 'bad' : ''}`}>
            <title>{tree?.hash ?? ''}</title>
            <rect x={w / 2 - 60} y={6} width={120} height={NH} rx={6} />
            <text className="t-cap" x={w / 2} y={19} textAnchor="middle">
              ROOT · EMPTY BATCH
            </text>
            <text className="t-hash" x={w / 2} y={34} textAnchor="middle">
              {tree?.hash.slice(0, 8) ?? ''}
            </text>
          </g>
        </svg>
      </div>
    );
  }

  const leaves: MerkleTreeNode[] = [];
  const walk = (n: MerkleTreeNode) => (isLeaf(n) ? leaves.push(n) : kids(n).forEach(walk));
  walk(tree);
  // Leaf row columns: the present leaves and the deleted slots, merged in original slot order.
  const slotOf = (n: MerkleTreeNode) => slots?.[n.leaf!] ?? n.leaf!;
  const row: { slot: number; node?: MerkleTreeNode }[] = [
    ...leaves.map((node) => ({ slot: slotOf(node), node })),
    ...missing.map((slot) => ({ slot })),
  ].sort((a, b) => a.slot - b.slot);
  const col = new Map<MerkleTreeNode, number>();
  row.forEach((r, i) => r.node && col.set(r.node, i));
  const colX = (i: number) => PAD_X + NW / 2 + i * COL_W;
  const heights = new Map<MerkleTreeNode, number>();
  const height = (n: MerkleTreeNode): number => {
    const cached = heights.get(n);
    if (cached !== undefined) return cached;
    const h = isLeaf(n) ? 0 : 1 + Math.max(...kids(n).map(height));
    heights.set(n, h);
    return h;
  };
  const top = height(tree);
  const pos = new Map<MerkleTreeNode, { x: number; y: number }>();
  const place = (n: MerkleTreeNode) => {
    const y = PAD_TOP + (top - height(n)) * ROW_H;
    if (isLeaf(n)) {
      pos.set(n, { x: colX(col.get(n)!), y });
      return;
    }
    const [l, r] = kids(n);
    place(l);
    place(r);
    pos.set(n, { x: (pos.get(l)!.x + pos.get(r)!.x) / 2, y });
  };
  place(tree);
  const isBad = (n: MerkleTreeNode): boolean => (isLeaf(n) ? bad.has(n.leaf!) : kids(n).some(isBad));
  const anim = (n: MerkleTreeNode) =>
    animate
      ? {
          className: 'tree-node-appear',
          style: { animationDelay: `${(height(n) * 0.45 + (isLeaf(n) ? n.leaf! * 0.06 : 0)).toFixed(2)}s` },
        }
      : { className: '', style: undefined };

  const lines: React.ReactNode[] = [];
  const nodes: React.ReactNode[] = [];
  const draw = (n: MerkleTreeNode, id: string) => {
    const p = pos.get(n)!;
    const leaf = isLeaf(n);
    const root = n === tree;
    const nBad = isBad(n) || (root && rootBad);
    const a = anim(n);
    if (!leaf) {
      kids(n).forEach((c, i) => {
        const q = pos.get(c)!;
        lines.push(
          <path
            key={`${id}-${i}`}
            className={`t-line ${isBad(c) ? 'bad' : ''} ${a.className}`}
            style={a.style}
            d={`M${q.x},${q.y} C${q.x},${q.y - ROW_H * 0.4} ${p.x},${p.y + NH + ROW_H * 0.25} ${p.x},${p.y + NH}`}
          />,
        );
        draw(c, `${id}${i}`);
      });
    }
    const cap = root ? 'ROOT' : leaf ? `LEAF ${slotOf(n) + 1}` : 'NODE';
    const label = leaf ? labels[n.leaf!] : undefined;
    nodes.push(
      <g
        key={id}
        className={`t-node ${leaf ? 'leaf' : ''} ${root ? 'root' : ''} ${nBad ? 'bad' : ''} ${a.className}`}
        style={a.style}
      >
        <title>{n.hash}</title>
        <rect x={p.x - NW / 2} y={p.y} width={NW} height={NH} rx={6} />
        <text className="t-cap" x={p.x} y={p.y + 13} textAnchor="middle">
          {cap}
        </text>
        <text className="t-hash" x={p.x} y={p.y + 28} textAnchor="middle">
          {n.hash.slice(0, 8)}
        </text>
        {label && (
          <text className={`t-key ${nBad ? 'bad' : ''}`} x={p.x} y={p.y + NH + 13} textAnchor="middle">
            {label}
          </text>
        )}
      </g>,
    );
  };
  draw(tree, 'n');
  const leafY = PAD_TOP + top * ROW_H;
  row.forEach((r, i) => {
    if (!r.node) nodes.push(<Ghost key={`gap-${r.slot}`} slot={r.slot} x={colX(i)} y={leafY} />);
  });
  const w = PAD_X * 2 + NW + (row.length - 1) * COL_W;
  const h = PAD_TOP + top * ROW_H + NH + KEY_H;
  return (
    <div className="tree-wrap">
      <svg
        viewBox={`0 0 ${w} ${h}`}
        style={{ width: '100%', maxWidth: w, height: 'auto', ['--tree-w' as string]: `${w}px` }}
      >
        {lines}
        {nodes}
      </svg>
    </div>
  );
}
