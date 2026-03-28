# API Specification

## Users

GET /api/users

POST /api/users
{
"name": "string"
}

PATCH /api/users/:id
{
"name": "string"
}

---

## Pulsoid Tokens

GET /api/users/:id/pulsoid-tokens

POST /api/users/:id/pulsoid-tokens
{
"label": "string",
"access_token": "string"
}

PATCH /api/pulsoid-tokens/:id
{
"label": "string",
"is_active": true
}

DELETE /api/pulsoid-tokens/:id

---

## Heart Rate

GET /api/users/:id/heart-rates?from=&to=&limit=

GET /api/users/:id/latest-heart-rate

Response:
{
"bpm": 78,
"timestamp": 1710000000
}