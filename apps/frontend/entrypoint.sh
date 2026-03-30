#!/bin/sh
if [ -d /static-export ]; then
  cp -r .next/static/* /static-export/
fi
exec node server.js
