const scores: { player: string; points: number }[] = [];

export async function GET() {
  return Response.json(scores);
}

export async function POST(req: Request) {
  scores.push(await req.json());
  return Response.json({ ok: true }, { status: 201 });
}
