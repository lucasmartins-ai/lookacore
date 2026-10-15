import requests

BASE = "http://localhost:3000"


def score(score_id: str) -> dict:
    return requests.get(f"{BASE}/api/scores/{score_id}").json()


def publish_leaderboard(rows: list) -> None:
    requests.post("http://localhost:3000/api/leaderboard", json=rows)
