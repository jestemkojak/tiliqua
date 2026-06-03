#[doc = "Register `palette` reader"]
pub type R = crate::R<PALETTE_SPEC>;
#[doc = "Register `palette` writer"]
pub type W = crate::W<PALETTE_SPEC>;
#[doc = "Field `position` writer - position field"]
pub type POSITION_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `red` writer - red field"]
pub type RED_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `green` writer - green field"]
pub type GREEN_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `blue` writer - blue field"]
pub type BLUE_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
impl W {
    #[doc = "Bits 0:7 - position field"]
    #[inline(always)]
    pub fn position(&mut self) -> POSITION_W<'_, PALETTE_SPEC> {
        POSITION_W::new(self, 0)
    }
    #[doc = "Bits 8:15 - red field"]
    #[inline(always)]
    pub fn red(&mut self) -> RED_W<'_, PALETTE_SPEC> {
        RED_W::new(self, 8)
    }
    #[doc = "Bits 16:23 - green field"]
    #[inline(always)]
    pub fn green(&mut self) -> GREEN_W<'_, PALETTE_SPEC> {
        GREEN_W::new(self, 16)
    }
    #[doc = "Bits 24:31 - blue field"]
    #[inline(always)]
    pub fn blue(&mut self) -> BLUE_W<'_, PALETTE_SPEC> {
        BLUE_W::new(self, 24)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`palette::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`palette::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct PALETTE_SPEC;
impl crate::RegisterSpec for PALETTE_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`palette::R`](R) reader structure"]
impl crate::Readable for PALETTE_SPEC {}
#[doc = "`write(|w| ..)` method takes [`palette::W`](W) writer structure"]
impl crate::Writable for PALETTE_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets palette to value 0"]
impl crate::Resettable for PALETTE_SPEC {}
