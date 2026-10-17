#!/bin/sh
# Pousser vers le depot public GitHub (acces par cle SSH, aucun jeton en clair).
#   sh scripts/push.sh
set -e
REMOTE=${REMOTE:-git@github.com:zagg/nvtop-rs.git}
BR=$(git symbolic-ref --short HEAD)
git remote add github "$REMOTE" 2>/dev/null || git remote set-url github "$REMOTE"
git push -u github "$BR"
echo "pousse : $(git rev-parse --short HEAD) -> $REMOTE#$BR"
