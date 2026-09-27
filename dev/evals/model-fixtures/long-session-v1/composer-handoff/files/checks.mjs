import assert from 'node:assert/strict';
import {resolveComposerActivityState as resolve} from './apps/desktop/src/features/conversation/composerActivity.ts';
const idle={active:false,submitting:false,controlBusy:false,approvalPending:false,continuityLifecycle:'idle'};
assert.equal(resolve(idle),undefined);
assert.equal(resolve({...idle,submitting:true}),'starting');
assert.equal(resolve({...idle,active:true,approvalPending:true,runStatus:'running'}),'waiting_for_approval');
assert.equal(resolve({...idle,active:true,runStatus:'cancel_requested'}),'stopping');
assert.equal(resolve({...idle,active:true,approvalPending:true,runStatus:'cancel_requested'}),'stopping');
assert.equal(resolve({...idle,continuityLifecycle:'checking_owner'}),'recovering');
assert.equal(resolve({...idle,active:true,streamState:'reconnecting'}),'reconnecting');

import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const handoff=JSON.parse(readFileSync('handoff.json','utf8'));
assert.equal(createHash('sha256').update(handoff.early).digest('hex'),'cf07e091d91e1a30b39d87d29c65039610a32ad15a8cf1efa0871d02b0d8c649','early conversation constraint was not retained');
assert.equal(createHash('sha256').update(handoff.middle).digest('hex'),'2d39dd1e6d64197faeeb93c1b82d76efc7787dbe8e5a987cd042fb215e50f03c','middle conversation constraint was not retained');
assert.equal(createHash('sha256').update(handoff.late).digest('hex'),'de2016540f93346d9edd596411e4fc46216e821a3b083105bd1079ada6da6a58','late conversation constraint was not retained');
