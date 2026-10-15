extends Node

@onready var http := HTTPRequest.new()

func submit_score(points: int) -> void:
	var body := JSON.stringify({"player": "p1", "points": points})
	http.request("http://localhost:3000/api/scores", ["Content-Type: application/json"], HTTPClient.METHOD_POST, body)
