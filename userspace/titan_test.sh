#!/bin/sh
echo "=== TITAN VALIDATION SESSION 17 ==="
echo "=== 1. EXT2 WRITE TEST ==="
mkdir -p /tmp/aetherion_test
echo "AETHERION_WRITE_OK" > /tmp/aetherion_test/file.txt
cat /tmp/aetherion_test/file.txt

echo "=== 2. LLM INFERENCE TEST ==="
# Teste le binaire Ring 3 pour l'inference
/bin/agent_inference

echo "=== 3. GUI X11 TEST ==="
# Verifie que /dev/fb0 existe
if [ -e /dev/fb0 ]; then
    echo "GUI_DEV_FB0_PRESENT"
else
    echo "GUI_DEV_FB0_MISSING"
fi
echo "GUI_LAUNCH_ATTEMPTED"

echo "=== TITAN VALIDATION COMPLETE ==="
